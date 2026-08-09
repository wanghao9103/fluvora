use std::collections::{HashSet, VecDeque};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use fluvora_auth::Scopes;
use fluvora_domain::{
    CustomData, Room, RoomCommand, RoomId, RoomPolicy, UserId, VerifiedGiftReceipt,
};

use super::media::{cleanup_participant_media, cleanup_publisher_tracks, cleanup_room_media};
use crate::error::{
    ApiError, control_store_unavailable, internal_error, lock_error, room_not_found,
};
use crate::gift::{GiftRequest, verify_gift_receipt};
use crate::models::{
    AppState, ChatRequest, CommandResponse, CreateRoomRequest, CustomDataRequest,
    RevokeTokenRequest, RoleRequest, RoomResponse, RoomSnapshotResponse,
};
use crate::persistence::{
    EVENT_CHANNEL_CAPACITY, ManagedRoom, RoomPersistence, persist_created_room,
};
use crate::runtime::{format_id, now_millis, random_u128};
use crate::services::{authenticate, execute_room_command, require_room_member};
use crate::signals::{append_signal, custom_signal_payload, validate_signal_payload};
use crate::validation::{
    idempotency_key, mode_name, parse_id, parse_mode, parse_role, parse_room_id,
};
use tokio::sync::broadcast;

pub(crate) async fn create_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRoomRequest>,
) -> Result<(StatusCode, Json<RoomResponse>), ApiError> {
    let claims = authenticate(&state, &headers, Scopes::ROOM_CREATE, None)?;
    let command_id = idempotency_key(&headers)?;
    let mode = parse_mode(&request.mode)?;
    let mut policy = RoomPolicy::default();
    if let Some(max_members) = request.max_members {
        policy.max_members = max_members.clamp(2, 100_000);
    }
    if let Some(max_publishers) = request.max_publishers {
        policy.max_publishers = max_publishers.clamp(1, 1_024);
    }
    let _mutation = state.room_mutations.lock().await;
    if let Some(room_id) = state
        .room_creations
        .read()
        .map_err(lock_error)?
        .get(&command_id)
        .copied()
    {
        let rooms = state.rooms.read().map_err(lock_error)?;
        let managed = rooms.get(&room_id).ok_or_else(room_not_found)?;
        return Ok((
            StatusCode::OK,
            Json(RoomResponse {
                room_id: format_id(room_id.0),
                mode: mode_name(managed.room.mode()).to_owned(),
                sequence: managed.room.sequence(),
                duplicate: true,
            }),
        ));
    }
    let room_id = RoomId(random_u128()?);
    let (room, event) = Room::create(
        room_id,
        mode,
        UserId(claims.subject),
        policy,
        command_id,
        now_millis(),
    );
    let managed = ManagedRoom {
        room,
        creation_event: event,
        persistence_revision: 1,
        signals: VecDeque::new(),
        signal_cache_bytes: 0,
        next_signal_sequence: 1,
        side_effect_commands: HashSet::new(),
        side_effect_order: VecDeque::new(),
    };
    let (room_id, managed, duplicate) =
        persist_created_room(&state.persistence, room_id, managed).await?;
    state
        .rooms
        .write()
        .map_err(lock_error)?
        .insert(room_id, managed.clone());
    state
        .event_channels
        .write()
        .map_err(lock_error)?
        .entry(room_id)
        .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0);
    state
        .room_creations
        .write()
        .map_err(lock_error)?
        .insert(command_id, room_id);
    if !duplicate {
        state.metrics.active_rooms.add(1);
    }
    Ok((
        if duplicate {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(RoomResponse {
            room_id: format_id(room_id.0),
            mode: mode_name(managed.room.mode()).to_owned(),
            sequence: managed.room.sequence(),
            duplicate,
        }),
    ))
}

pub(crate) async fn get_room(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<RoomSnapshotResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let managed = rooms.get(&room_id).ok_or_else(room_not_found)?;
    Ok(Json(RoomSnapshotResponse {
        room_id: format_id(room_id.0),
        mode: mode_name(managed.room.mode()),
        sequence: managed.room.sequence(),
        ended: managed.room.is_ended(),
        member_count: managed.room.member_count(),
        publisher_count: managed.room.publisher_count(),
    }))
}

pub(crate) async fn join_room(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    let command_id = idempotency_key(&headers)?;
    let response = execute_room_command(
        &state,
        room_id,
        command_id,
        RoomCommand::Join {
            user: UserId(claims.subject),
        },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn leave_room(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    let response = execute_room_command(
        &state,
        room_id,
        idempotency_key(&headers)?,
        RoomCommand::Leave {
            user: UserId(claims.subject),
        },
    )
    .await?;
    cleanup_participant_media(&state, room_id, claims.subject).await;
    Ok(Json(response))
}

pub(crate) async fn end_room(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(
        &state,
        &headers,
        Scopes::ROOM_JOIN.union(Scopes::ROOM_MODERATE),
        Some(room_id),
    )?;
    let response = execute_room_command(
        &state,
        room_id,
        idempotency_key(&headers)?,
        RoomCommand::End {
            actor: UserId(claims.subject),
        },
    )
    .await?;
    if !response.duplicate {
        state.metrics.active_rooms.add(-1);
    }
    cleanup_room_media(&state, room_id).await;
    if let RoomPersistence::Postgres(store) = state.persistence.as_ref()
        && let Err(error) = store.remove_room_placement(&format_id(room_id.0)).await
    {
        eprintln!("failed to remove ended room placement: {error}");
    }
    Ok(Json(response))
}

pub(crate) async fn set_role(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RoleRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(
        &state,
        &headers,
        Scopes::ROOM_JOIN.union(Scopes::ROOM_MODERATE),
        Some(room_id),
    )?;
    let response = execute_room_command(
        &state,
        room_id,
        idempotency_key(&headers)?,
        RoomCommand::SetRole {
            actor: UserId(claims.subject),
            user: UserId(parse_id(&request.user_id)?),
            role: parse_role(&request.role)?,
        },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn record_gift(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<GiftRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    authenticate(&state, &headers, Scopes::GIFT_VERIFY, Some(room_id))?;
    verify_gift_receipt(&state.gift_webhook_secret, room_id, &request, now_millis())?;
    let sender = parse_id(&request.sender_id)?;
    let command_id = idempotency_key(&headers)?;
    let response = execute_room_command(
        &state,
        room_id,
        command_id,
        RoomCommand::RecordVerifiedGift {
            sender: UserId(sender),
            receipt: VerifiedGiftReceipt {
                transaction_id: request.transaction_id.clone(),
                gift_id: request.gift_id.clone(),
                quantity: request.quantity,
                unit_value: request.unit_value,
                currency: request.currency.clone(),
                recipient: UserId(parse_id(&request.recipient_id)?),
            },
        },
    )
    .await?;
    append_signal(
        &state,
        room_id,
        command_id,
        sender,
        None,
        "room.gift".to_owned(),
        serde_json::json!({
            "sender_id": request.sender_id,
            "recipient_id": request.recipient_id,
            "transaction_id": request.transaction_id,
            "gift_id": request.gift_id,
            "quantity": request.quantity,
            "unit_value": request.unit_value,
            "currency": request.currency,
            "event_sequence": response.sequence
        }),
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn revoke_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RevokeTokenRequest>,
) -> Result<StatusCode, ApiError> {
    authenticate(&state, &headers, Scopes::TOKEN_REVOKE, None)?;
    let subject = parse_id(&request.subject_id)?;
    if request.expires_at_millis <= now_millis()
        || request.reason.is_empty()
        || request.reason.len() > 512
        || request.reason.chars().any(char::is_control)
    {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_token_revocation",
            message: "revocation expiration/reason is invalid".to_owned(),
        });
    }
    if let RoomPersistence::Postgres(store) = state.persistence.as_ref() {
        store
            .revoke_access_token(
                &format_id(subject),
                request.nonce,
                request.expires_at_millis,
                &request.reason,
            )
            .await
            .map_err(control_store_unavailable)?;
    }
    state
        .revoked_tokens
        .write()
        .map_err(lock_error)?
        .insert((subject, request.nonce), request.expires_at_millis);
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn send_chat(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ChatRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    let command_id = idempotency_key(&headers)?;
    let message_id = parse_id(&request.message_id)?;
    let response = execute_room_command(
        &state,
        room_id,
        command_id,
        RoomCommand::SendChat {
            user: UserId(claims.subject),
            message_id,
            text: request.text.clone(),
        },
    )
    .await?;
    append_signal(
        &state,
        room_id,
        command_id,
        claims.subject,
        None,
        "room.chat".to_owned(),
        serde_json::json!({
            "message_id": request.message_id,
            "text": request.text,
            "event_sequence": response.sequence
        }),
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn send_custom_data(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CustomDataRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    let command_id = idempotency_key(&headers)?;
    validate_signal_payload(&custom_signal_payload(&request, u64::MAX))?;
    let payload = serde_json::to_vec(&request.payload).map_err(internal_error)?;
    let response = execute_room_command(
        &state,
        room_id,
        command_id,
        RoomCommand::SendCustomData {
            user: UserId(claims.subject),
            data: CustomData {
                namespace: request.namespace.clone(),
                schema_version: request.schema_version,
                payload,
            },
        },
    )
    .await?;
    append_signal(
        &state,
        room_id,
        command_id,
        claims.subject,
        None,
        "room.custom".to_owned(),
        custom_signal_payload(&request, response.sequence),
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn start_publishing(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(
        &state,
        &headers,
        Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH),
        Some(room_id),
    )?;
    let command_id = idempotency_key(&headers)?;
    let response = execute_room_command(
        &state,
        room_id,
        command_id,
        RoomCommand::StartPublishing {
            user: UserId(claims.subject),
        },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn stop_publishing(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(
        &state,
        &headers,
        Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH),
        Some(room_id),
    )?;
    let response = execute_room_command(
        &state,
        room_id,
        idempotency_key(&headers)?,
        RoomCommand::StopPublishing {
            user: UserId(claims.subject),
        },
    )
    .await?;
    cleanup_publisher_tracks(&state, room_id, claims.subject).await;
    Ok(Json(response))
}
