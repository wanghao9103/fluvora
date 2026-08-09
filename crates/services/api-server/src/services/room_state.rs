use axum::http::StatusCode;
use fluvora_domain::RoomId;
use tokio::sync::broadcast;

use crate::error::{ApiError, lock_error, room_not_found};
use crate::models::AppState;
use crate::persistence::{
    EVENT_CHANNEL_CAPACITY, PersistedRoom, RoomPersistence, managed_from_persisted,
    persisted_from_stored,
};
use crate::runtime::format_id;

pub(crate) async fn refresh_postgres_room(
    state: &AppState,
    room_id: RoomId,
) -> Result<(), ApiError> {
    let RoomPersistence::Postgres(store) = state.persistence.as_ref() else {
        return Ok(());
    };
    let stored = store
        .load_room(&format_id(room_id.0))
        .await
        .map_err(ApiError::from)?
        .ok_or_else(room_not_found)?;
    replace_durable_room(state, room_id, persisted_from_stored(stored)?)
}

pub(super) fn replace_durable_room(
    state: &AppState,
    room_id: RoomId,
    persisted: PersistedRoom,
) -> Result<(), ApiError> {
    let creation_command = persisted.creation_command_id().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "corrupt_room_snapshot",
        message: "durable room has no creation event".to_owned(),
    })?;
    let mut refreshed = managed_from_persisted(persisted)?;
    let mut rooms = state.rooms.write().map_err(lock_error)?;
    if let Some(existing) = rooms.get(&room_id) {
        refreshed.signals.clone_from(&existing.signals);
        refreshed.signal_cache_bytes = existing.signal_cache_bytes;
        refreshed.next_signal_sequence = existing.next_signal_sequence;
    }
    rooms.insert(room_id, refreshed);
    drop(rooms);
    state
        .room_creations
        .write()
        .map_err(lock_error)?
        .insert(creation_command, room_id);
    state
        .event_channels
        .write()
        .map_err(lock_error)?
        .entry(room_id)
        .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0);
    Ok(())
}
