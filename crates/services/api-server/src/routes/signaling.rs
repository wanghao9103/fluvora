use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use fluvora_auth::Scopes;
use fluvora_domain::{RoomId, RoomMode};
use tokio::sync::broadcast;

use crate::error::{ApiError, lock_error, room_not_found, unauthorized};
use crate::models::{
    AppState, EventQuery, EventTicket, EventTicketResponse, IceServer, IceServersResponse,
    SignalQuery, SignalRecord, SignalRequest, SignalResponse,
};
use crate::runtime::{EVENT_TICKET_TTL_MILLIS, format_id, now_millis, random_u128};
use crate::services::{authenticate, require_room_member, require_room_mode};
use crate::signals::{MAX_SIGNAL_PAGE_MESSAGES, append_signal, load_signal_page};
use crate::validation::{idempotency_key, parse_id, parse_room_id, validate_signal_kind};

pub(crate) async fn post_signal(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SignalRequest>,
) -> Result<(StatusCode, Json<SignalRecord>), ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_mode(&state, room_id, RoomMode::P2p)?;
    require_room_member(&state, room_id, claims.subject)?;
    validate_signal_kind(&request.kind)?;
    let command_id = idempotency_key(&headers)?;
    let to = request.to.as_deref().map(parse_id).transpose()?;
    let signal = append_signal(
        &state,
        room_id,
        command_id,
        claims.subject,
        to,
        request.kind,
        request.payload,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(signal)))
}

pub(crate) async fn get_signals(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<SignalQuery>,
    headers: HeaderMap,
) -> Result<Json<SignalResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    let after = query.after.unwrap_or_default();
    let limit = query
        .limit
        .unwrap_or(100)
        .clamp(1, MAX_SIGNAL_PAGE_MESSAGES);
    Ok(Json(
        load_signal_page(&state, room_id, after, limit, claims.subject).await?,
    ))
}

pub(crate) async fn room_events(
    websocket: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let participant_id = if let Some(ticket) = query.ticket.as_deref() {
        consume_event_ticket(&state, room_id, ticket)?
    } else {
        authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?.subject
    };
    require_room_member(&state, room_id, participant_id)?;
    let receiver = state
        .event_channels
        .read()
        .map_err(lock_error)?
        .get(&room_id)
        .ok_or_else(room_not_found)?
        .subscribe();
    let after = query.after.unwrap_or_default();
    let limit = query
        .limit
        .unwrap_or(MAX_SIGNAL_PAGE_MESSAGES)
        .clamp(1, MAX_SIGNAL_PAGE_MESSAGES);
    let participant = format_id(participant_id);
    let replay = load_signal_page(&state, room_id, after, limit, participant_id)
        .await?
        .signals;
    Ok(websocket
        .on_upgrade(move |socket| stream_room_events(socket, receiver, replay, participant))
        .into_response())
}

pub(crate) async fn issue_event_ticket(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<EventTicketResponse>), ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    let ticket = format_id(random_u128()?);
    let expires_at_millis = now_millis().saturating_add(EVENT_TICKET_TTL_MILLIS);
    let mut tickets = state.event_tickets.write().map_err(lock_error)?;
    let now = now_millis();
    tickets.retain(|_, value| value.expires_at_millis > now);
    if tickets.len() >= 100_000 {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "event_ticket_capacity",
            message: "event ticket capacity reached".to_owned(),
        });
    }
    tickets.insert(
        ticket.clone(),
        EventTicket {
            room_id,
            participant: claims.subject,
            expires_at_millis,
        },
    );
    Ok((
        StatusCode::CREATED,
        Json(EventTicketResponse {
            ticket,
            expires_at_millis,
        }),
    ))
}

pub(crate) async fn get_ice_servers(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<IceServersResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    let expires_at_seconds = now_millis()
        .checked_div(1_000)
        .and_then(|now| now.checked_add(3_600))
        .ok_or_else(|| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "credential_time_exhausted",
            message: "cannot issue TURN credential".to_owned(),
        })?;
    let username = format!("{expires_at_seconds}:{}", format_id(claims.subject));
    let credential = fluvora_turn::rest_credential_password(&state.turn_rest_secret, &username)
        .map_err(|error| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "turn_credential_unavailable",
            message: error.to_string(),
        })?;
    Ok(Json(IceServersResponse {
        ice_servers: vec![IceServer {
            urls: state.ice_urls.as_ref().clone(),
            username,
            credential,
        }],
        expires_at_millis: expires_at_seconds.saturating_mul(1_000),
    }))
}

fn consume_event_ticket(state: &AppState, room_id: RoomId, ticket: &str) -> Result<u128, ApiError> {
    if ticket.len() != 32 || !ticket.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unauthorized());
    }
    let event_ticket = state
        .event_tickets
        .write()
        .map_err(lock_error)?
        .remove(ticket)
        .ok_or_else(unauthorized)?;
    if event_ticket.room_id != room_id || event_ticket.expires_at_millis <= now_millis() {
        return Err(unauthorized());
    }
    Ok(event_ticket.participant)
}

async fn stream_room_events(
    mut socket: WebSocket,
    mut receiver: broadcast::Receiver<SignalRecord>,
    replay: Vec<SignalRecord>,
    participant: String,
) {
    let mut cursor = replay
        .first()
        .map_or(0, |event| event.sequence.saturating_sub(1));
    for event in replay {
        cursor = cursor.max(event.sequence);
        if send_websocket_event(&mut socket, &event).await.is_err() {
            return;
        }
    }
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(event) => {
                    if event.sequence > cursor
                        && event
                            .to
                            .as_deref()
                            .is_none_or(|recipient| recipient == participant)
                    {
                        cursor = event.sequence;
                        if send_websocket_event(&mut socket, &event).await.is_err() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let notice = serde_json::json!({
                        "kind": "system.resync_required",
                        "after": cursor,
                    });
                    if socket.send(Message::Text(notice.to_string().into())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(Message::Ping(bytes))) => {
                    if socket.send(Message::Pong(bytes)).await.is_err() {
                        return;
                    }
                }
                Some(Ok(_)) => {}
            },
            _ = heartbeat.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn send_websocket_event(
    socket: &mut WebSocket,
    event: &SignalRecord,
) -> Result<(), axum::Error> {
    let payload = serde_json::to_string(event).map_err(axum::Error::new)?;
    socket.send(Message::Text(payload.into())).await
}
