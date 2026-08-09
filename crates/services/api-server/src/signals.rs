use std::collections::VecDeque;

use axum::http::StatusCode;
use fluvora_control_store::StoredSignal;
use fluvora_domain::{CommandId, RoomId};

use crate::error::{ApiError, internal_error, lock_error, room_not_found};
use crate::models::{AppState, CustomDataRequest, SignalRecord, SignalResponse};
use crate::persistence::RoomPersistence;
use crate::runtime::{format_id, now_millis};
use crate::validation::{parse_id, parse_room_id};

pub(super) const MAX_JSON_REQUEST_BYTES: usize = 1024 * 1024;
pub(super) const MAX_SIGNAL_PAGE_MESSAGES: usize = 128;
const MAX_SIGNAL_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_SIGNAL_CACHE_BYTES: usize = 8 * 1024 * 1024;
const SIGNAL_BACKLOG: usize = MAX_SIGNAL_PAGE_MESSAGES;

pub(super) fn custom_signal_payload(
    request: &CustomDataRequest,
    event_sequence: u64,
) -> serde_json::Value {
    serde_json::json!({
        "namespace": &request.namespace,
        "schema_version": request.schema_version,
        "payload": &request.payload,
        "event_sequence": event_sequence
    })
}

pub(super) fn validate_signal_payload(payload: &serde_json::Value) -> Result<(), ApiError> {
    let encoded_len = serde_json::to_vec(payload).map_err(internal_error)?.len();
    if encoded_len <= MAX_SIGNAL_PAYLOAD_BYTES {
        return Ok(());
    }
    Err(ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "signal_too_large",
        message: format!("signal payload exceeds {MAX_SIGNAL_PAYLOAD_BYTES} bytes"),
    })
}

pub(super) async fn append_signal(
    state: &AppState,
    room_id: RoomId,
    command_id: CommandId,
    from: u128,
    to: Option<u128>,
    kind: String,
    payload: serde_json::Value,
) -> Result<SignalRecord, ApiError> {
    validate_signal_payload(&payload)?;
    let file_guard = match state.persistence.as_ref() {
        RoomPersistence::Files(_) => Some(state.room_mutations.lock().await),
        RoomPersistence::Postgres(_) => None,
    };
    let signal = match state.persistence.as_ref() {
        RoomPersistence::Postgres(store) => signal_record(
            room_id,
            store
                .append_room_signal(&StoredSignal {
                    room_id: format_id(room_id.0),
                    sequence: 0,
                    command_id: format_id(command_id.0),
                    from_id: format_id(from),
                    to_id: to.map(format_id),
                    kind,
                    payload,
                    timestamp_millis: now_millis(),
                })
                .await
                .map_err(ApiError::from)?,
        )?,
        RoomPersistence::Files(_) => {
            let rooms = state.rooms.read().map_err(lock_error)?;
            let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
            if let Some(existing) = room
                .signals
                .iter()
                .find(|signal| signal.command_id == Some(command_id))
            {
                return Ok(existing.clone());
            }
            SignalRecord {
                command_id: Some(command_id),
                sequence: room.next_signal_sequence,
                from: format_id(from),
                to: to.map(format_id),
                kind,
                payload,
                timestamp_millis: now_millis(),
            }
        }
    };
    cache_signal(state, room_id, signal.clone())?;
    drop(file_guard);
    Ok(signal)
}

pub(super) fn cache_signal(
    state: &AppState,
    room_id: RoomId,
    signal: SignalRecord,
) -> Result<(), ApiError> {
    let encoded_bytes = signal_cache_bytes(&signal);
    let inserted = {
        let mut rooms = state.rooms.write().map_err(lock_error)?;
        let room = rooms.get_mut(&room_id).ok_or_else(room_not_found)?;
        if room
            .signals
            .iter()
            .any(|existing| existing.sequence == signal.sequence)
        {
            false
        } else {
            room.next_signal_sequence = room
                .next_signal_sequence
                .max(signal.sequence.saturating_add(1));
            room.signals.push_back(signal.clone());
            room.signal_cache_bytes = room.signal_cache_bytes.saturating_add(encoded_bytes);
            room.signals
                .make_contiguous()
                .sort_by_key(|record| record.sequence);
            trim_signal_cache(&mut room.signals, &mut room.signal_cache_bytes);
            true
        }
    };
    if inserted
        && let Some(sender) = state
            .event_channels
            .read()
            .map_err(lock_error)?
            .get(&room_id)
    {
        let _ = sender.send(signal);
    }
    Ok(())
}

fn trim_signal_cache(signals: &mut VecDeque<SignalRecord>, cached_bytes: &mut usize) {
    while signals.len() > SIGNAL_BACKLOG || *cached_bytes > MAX_SIGNAL_CACHE_BYTES {
        let Some(expired) = signals.pop_front() else {
            break;
        };
        *cached_bytes = cached_bytes.saturating_sub(signal_cache_bytes(&expired));
    }
}

fn signal_cache_bytes(signal: &SignalRecord) -> usize {
    serde_json::to_vec(signal).map_or(usize::MAX, |encoded| encoded.len())
}

pub(super) fn signal_record(
    room_id: RoomId,
    signal: StoredSignal,
) -> Result<SignalRecord, ApiError> {
    if parse_room_id(&signal.room_id)? != room_id {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "corrupt_signal_room",
            message: "durable signal room identifier does not match its stream".to_owned(),
        });
    }
    let from = parse_id(&signal.from_id)?;
    let to = signal.to_id.as_deref().map(parse_id).transpose()?;
    Ok(SignalRecord {
        command_id: Some(CommandId(parse_id(&signal.command_id)?)),
        sequence: signal.sequence,
        from: format_id(from),
        to: to.map(format_id),
        kind: signal.kind,
        payload: signal.payload,
        timestamp_millis: signal.timestamp_millis,
    })
}

pub(super) async fn load_signal_page(
    state: &AppState,
    room_id: RoomId,
    after: u64,
    limit: usize,
    participant: u128,
) -> Result<SignalResponse, ApiError> {
    match state.persistence.as_ref() {
        RoomPersistence::Postgres(store) => {
            let maximum_messages = u32::try_from(limit).map_err(internal_error)?;
            let page = store
                .load_room_signal_page(
                    &format_id(room_id.0),
                    after,
                    maximum_messages,
                    &format_id(participant),
                )
                .await
                .map_err(ApiError::from)?;
            Ok(SignalResponse {
                signals: page
                    .signals
                    .into_iter()
                    .map(|signal| signal_record(room_id, signal))
                    .collect::<Result<_, _>>()?,
                latest_sequence: page.latest_sequence,
            })
        }
        RoomPersistence::Files(_) => {
            let rooms = state.rooms.read().map_err(lock_error)?;
            let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
            let participant = format_id(participant);
            Ok(SignalResponse {
                signals: room
                    .signals
                    .iter()
                    .filter(|signal| {
                        signal.sequence > after
                            && signal.to.as_deref().is_none_or(|to| to == participant)
                    })
                    .take(limit)
                    .cloned()
                    .collect(),
                latest_sequence: room.next_signal_sequence.saturating_sub(1),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(sequence: u64, payload_bytes: usize) -> SignalRecord {
        SignalRecord {
            command_id: None,
            sequence,
            from: "1".repeat(32),
            to: None,
            kind: "offer".to_owned(),
            payload: serde_json::json!({ "data": "x".repeat(payload_bytes) }),
            timestamp_millis: 0,
        }
    }

    #[test]
    fn validates_custom_wrapper_before_room_command() {
        let request = CustomDataRequest {
            namespace: "com.example.state".to_owned(),
            schema_version: 1,
            payload: serde_json::json!("x".repeat(60 * 1024)),
        };
        validate_signal_payload(&custom_signal_payload(&request, u64::MAX)).unwrap();

        let oversized = CustomDataRequest {
            payload: serde_json::json!("x".repeat(MAX_SIGNAL_PAYLOAD_BYTES)),
            ..request
        };
        let error = validate_signal_payload(&custom_signal_payload(&oversized, u64::MAX))
            .expect_err("wrapper must be bounded");
        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn signal_cache_has_count_and_byte_budgets() {
        let mut many_small: VecDeque<SignalRecord> = (0..=SIGNAL_BACKLOG as u64)
            .map(|sequence| signal(sequence, 1))
            .collect();
        let mut small_bytes = many_small.iter().map(signal_cache_bytes).sum();
        trim_signal_cache(&mut many_small, &mut small_bytes);
        assert_eq!(many_small.len(), SIGNAL_BACKLOG);
        assert_eq!(many_small.front().unwrap().sequence, 1);

        let mut many_large: VecDeque<SignalRecord> = (0..256)
            .map(|sequence| signal(sequence, 64 * 1024))
            .collect();
        let mut large_bytes = many_large.iter().map(signal_cache_bytes).sum();
        trim_signal_cache(&mut many_large, &mut large_bytes);
        assert!(large_bytes <= MAX_SIGNAL_CACHE_BYTES);
        assert_eq!(
            large_bytes,
            many_large.iter().map(signal_cache_bytes).sum::<usize>()
        );
        assert!(many_large.len() < 256);
    }
}
