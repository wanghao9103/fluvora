use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fluvora_auth::{Claims, Scopes};
use fluvora_domain::{CommandId, CommandOutcome, RoomCommand, RoomId, RoomMode, UserId};

use super::room_state::{refresh_postgres_room, replace_durable_room};
use crate::error::{
    ApiError, control_store_unavailable, domain_error, forbidden, lock_error, room_not_found,
    unauthorized,
};
use crate::models::{AppState, CommandResponse};
use crate::persistence::{
    ManagedRoom, PersistAppendOutcome, RoomPersistence, SIDE_EFFECT_HISTORY_LIMIT,
    persist_appended_room, persist_managed_room,
};
use crate::runtime::{format_id, now_millis};

pub(crate) async fn execute_room_command(
    state: &AppState,
    room_id: RoomId,
    command_id: CommandId,
    command: RoomCommand,
) -> Result<CommandResponse, ApiError> {
    let _mutation = state.room_mutations.lock().await;
    for _ in 0..4 {
        refresh_postgres_room(state, room_id).await?;
        let mut candidate = state
            .rooms
            .read()
            .map_err(lock_error)?
            .get(&room_id)
            .cloned()
            .ok_or_else(room_not_found)?;
        let expected_revision = candidate.persistence_revision;
        match candidate
            .room
            .execute(command_id, now_millis(), command.clone())
            .map_err(|error| domain_error(&error))?
        {
            CommandOutcome::Applied(event) => {
                let sequence = event.sequence;
                candidate.persistence_revision = candidate
                    .persistence_revision
                    .checked_add(1)
                    .ok_or_else(|| ApiError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        code: "persistence_revision_exhausted",
                        message: "room persistence revision exhausted".to_owned(),
                    })?;
                match persist_appended_room(
                    &state.persistence,
                    room_id,
                    expected_revision,
                    &candidate,
                    &event,
                )
                .await?
                {
                    PersistAppendOutcome::Applied => {
                        state
                            .rooms
                            .write()
                            .map_err(lock_error)?
                            .insert(room_id, candidate);
                        return Ok(CommandResponse {
                            sequence,
                            duplicate: false,
                        });
                    }
                    PersistAppendOutcome::Duplicate(persisted) => {
                        replace_durable_room(state, room_id, *persisted)?;
                        let sequence = state
                            .rooms
                            .read()
                            .map_err(lock_error)?
                            .get(&room_id)
                            .ok_or_else(room_not_found)?
                            .room
                            .sequence();
                        return Ok(CommandResponse {
                            sequence,
                            duplicate: true,
                        });
                    }
                    PersistAppendOutcome::RevisionConflict => {}
                }
            }
            CommandOutcome::Duplicate => {
                return Ok(CommandResponse {
                    sequence: candidate.room.sequence(),
                    duplicate: true,
                });
            }
        }
    }
    Err(ApiError {
        status: StatusCode::CONFLICT,
        code: "room_revision_conflict",
        message: "room changed concurrently; retry with the same idempotency key".to_owned(),
    })
}

pub(crate) fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    scopes: Scopes,
    room_id: Option<RoomId>,
) -> Result<Claims, ApiError> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(unauthorized)?;
    let claims = state
        .tokens
        .verify(token, now_millis())
        .map_err(|_| unauthorized())?;
    if !claims.scopes.contains(scopes)
        || room_id.is_some_and(|room| claims.room_id != 0 && claims.room_id != room.0)
    {
        return Err(forbidden());
    }
    Ok(claims)
}

pub(crate) async fn reject_revoked_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(claims) = token.and_then(|token| state.tokens.verify(token, now_millis()).ok()) else {
        let response = next.run(request).await;
        state
            .metrics
            .control_processing_micros
            .observe_micros(elapsed_micros(started));
        return response;
    };
    let response = match access_token_revoked(&state, claims).await {
        Ok(false) => next.run(request).await,
        Ok(true) => unauthorized().into_response(),
        Err(error) => error.into_response(),
    };
    state
        .metrics
        .control_processing_micros
        .observe_micros(elapsed_micros(started));
    response
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

async fn access_token_revoked(state: &AppState, claims: Claims) -> Result<bool, ApiError> {
    let now = now_millis();
    let locally_revoked = {
        let mut revoked = state.revoked_tokens.write().map_err(lock_error)?;
        revoked.retain(|_, expires_at| *expires_at > now);
        revoked.contains_key(&(claims.subject, claims.nonce))
    };
    if locally_revoked {
        return Ok(true);
    }
    let RoomPersistence::Postgres(store) = state.persistence.as_ref() else {
        return Ok(false);
    };
    store
        .is_access_token_revoked(&format_id(claims.subject), claims.nonce)
        .await
        .map_err(control_store_unavailable)
}

pub(crate) fn require_room_member(
    state: &AppState,
    room_id: RoomId,
    user_id: u128,
) -> Result<(), ApiError> {
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
    if room.room.member_role(UserId(user_id)).is_some() {
        Ok(())
    } else {
        Err(forbidden())
    }
}

pub(crate) fn require_publishing(
    state: &AppState,
    room_id: RoomId,
    user_id: u128,
) -> Result<(), ApiError> {
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
    if room.room.is_publishing(UserId(user_id)) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "publisher_not_started",
            message: "start publishing before registering media tracks".to_owned(),
        })
    }
}

pub(crate) async fn side_effect_applied(
    state: &AppState,
    room_id: RoomId,
    command_id: CommandId,
) -> Result<bool, ApiError> {
    if let RoomPersistence::Postgres(store) = state.persistence.as_ref() {
        return store
            .side_effect_exists(&format_id(room_id.0), &format_id(command_id.0))
            .await
            .map_err(ApiError::from);
    }
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
    Ok(room.side_effect_commands.contains(&command_id))
}

pub(crate) async fn remember_side_effect(
    state: &AppState,
    room_id: RoomId,
    command_id: CommandId,
) -> Result<(), ApiError> {
    let _mutation = state.room_mutations.lock().await;
    if let RoomPersistence::Postgres(store) = state.persistence.as_ref() {
        store
            .mark_side_effect(&format_id(room_id.0), &format_id(command_id.0))
            .await
            .map_err(ApiError::from)?;
        let mut rooms = state.rooms.write().map_err(lock_error)?;
        let room = rooms.get_mut(&room_id).ok_or_else(room_not_found)?;
        remember_bounded_side_effect(room, command_id);
        return Ok(());
    }
    let mut rooms = state
        .rooms
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let room = rooms.get_mut(&room_id).ok_or_else(room_not_found)?;
    let inserted = room.side_effect_commands.insert(command_id);
    if inserted {
        room.side_effect_order.push_back(command_id);
        room.persistence_revision =
            room.persistence_revision
                .checked_add(1)
                .ok_or_else(|| ApiError {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    code: "persistence_revision_exhausted",
                    message: "room persistence revision exhausted".to_owned(),
                })?;
    }
    prune_side_effects(room);
    if inserted {
        let RoomPersistence::Files(directory) = state.persistence.as_ref() else {
            return Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "invalid_persistence_backend",
                message: "unexpected room persistence backend".to_owned(),
            });
        };
        persist_managed_room(directory, room_id, room)?;
    }
    Ok(())
}

fn remember_bounded_side_effect(room: &mut ManagedRoom, command_id: CommandId) {
    if room.side_effect_commands.insert(command_id) {
        room.side_effect_order.push_back(command_id);
    }
    prune_side_effects(room);
}

fn prune_side_effects(room: &mut ManagedRoom) {
    while room.side_effect_order.len() > SIDE_EFFECT_HISTORY_LIMIT {
        if let Some(expired) = room.side_effect_order.pop_front() {
            room.side_effect_commands.remove(&expired);
        }
    }
}

pub(crate) fn require_room_mode(
    state: &AppState,
    room_id: RoomId,
    expected: RoomMode,
) -> Result<(), ApiError> {
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
    if room.room.mode() == expected {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "wrong_room_mode",
            message: "operation is not valid for this room mode".to_owned(),
        })
    }
}

pub(crate) fn require_realtime_server_room(
    state: &AppState,
    room_id: RoomId,
) -> Result<(), ApiError> {
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let room = rooms.get(&room_id).ok_or_else(room_not_found)?;
    if matches!(room.room.mode(), RoomMode::Sfu | RoomMode::Live) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "wrong_room_mode",
            message: "server WebRTC offer is valid only for sfu and live rooms".to_owned(),
        })
    }
}
