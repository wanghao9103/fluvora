use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use fluvora_control_store::{
    AppendOutcome as StoreAppendOutcome, CreateRoomOutcome as StoreCreateOutcome, EventWrite,
    GiftLedgerWrite, PostgresStore, StoredRoom,
};
use fluvora_domain::{
    CommandId, MemberRole, Room, RoomEvent, RoomEventKind, RoomId, RoomMode, RoomPolicy,
    RoomSnapshot,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::{ApiError, internal_error, state_io_error};
use crate::models::SignalRecord;

pub(super) const EVENT_CHANNEL_CAPACITY: usize = 128;
const MAX_ROOM_SNAPSHOT_BYTES: u64 = 32 * 1_024 * 1_024;
pub(super) const SIDE_EFFECT_HISTORY_LIMIT: usize = 4_096;

#[derive(Debug, Clone)]
pub(super) struct ManagedRoom {
    pub(super) room: Room,
    pub(super) creation_event: RoomEvent,
    pub(super) persistence_revision: u64,
    pub(super) signals: VecDeque<SignalRecord>,
    pub(super) signal_cache_bytes: usize,
    pub(super) next_signal_sequence: u64,
    pub(super) side_effect_commands: HashSet<CommandId>,
    pub(super) side_effect_order: VecDeque<CommandId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PersistedRoom {
    room_id: RoomId,
    mode: RoomMode,
    policy: RoomPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<RoomEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<RoomSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    creation_event: Option<RoomEvent>,
    persistence_revision: u64,
    side_effect_order: Vec<CommandId>,
}

impl PersistedRoom {
    pub(super) fn creation_command_id(&self) -> Option<CommandId> {
        self.creation_event().map(|event| event.command_id)
    }

    fn creation_event(&self) -> Option<&RoomEvent> {
        self.creation_event.as_ref().or_else(|| self.events.first())
    }
}

#[derive(Debug)]
pub(super) struct LoadedRooms {
    pub(super) rooms: HashMap<RoomId, ManagedRoom>,
    pub(super) room_creations: HashMap<CommandId, RoomId>,
    pub(super) event_channels: HashMap<RoomId, broadcast::Sender<SignalRecord>>,
}

#[derive(Debug, Clone)]
pub(super) enum RoomPersistence {
    Files(Arc<PathBuf>),
    Postgres(PostgresStore),
}

#[derive(Debug)]
pub(super) enum PersistAppendOutcome {
    Applied,
    Duplicate(Box<PersistedRoom>),
    RevisionConflict,
}

pub(super) async fn persist_created_room(
    persistence: &RoomPersistence,
    room_id: RoomId,
    managed: ManagedRoom,
) -> Result<(RoomId, ManagedRoom, bool), ApiError> {
    match persistence {
        RoomPersistence::Files(directory) => {
            persist_managed_room(directory, room_id, &managed)?;
            Ok((room_id, managed, false))
        }
        RoomPersistence::Postgres(store) => {
            let stored = stored_room(room_id, &managed)?;
            let event = event_write(&managed.creation_event)?;
            match store
                .create_room(&stored, &event)
                .await
                .map_err(ApiError::from)?
            {
                StoreCreateOutcome::Created => Ok((room_id, managed, false)),
                StoreCreateOutcome::Duplicate(existing) => {
                    let persisted = persisted_from_stored(existing)?;
                    let room_id = persisted.room_id;
                    Ok((room_id, managed_from_persisted(persisted)?, true))
                }
            }
        }
    }
}

pub(super) async fn persist_appended_room(
    persistence: &RoomPersistence,
    room_id: RoomId,
    expected_revision: u64,
    managed: &ManagedRoom,
    event: &RoomEvent,
) -> Result<PersistAppendOutcome, ApiError> {
    match persistence {
        RoomPersistence::Files(directory) => {
            if managed.persistence_revision != expected_revision.saturating_add(1) {
                return Ok(PersistAppendOutcome::RevisionConflict);
            }
            persist_managed_room(directory, room_id, managed)?;
            Ok(PersistAppendOutcome::Applied)
        }
        RoomPersistence::Postgres(store) => {
            let stored = stored_room(room_id, managed)?;
            let event_write = event_write(event)?;
            let gift = gift_ledger_write(event);
            match store
                .append_room_event(&stored, expected_revision, &event_write, gift.as_ref())
                .await
                .map_err(ApiError::from)?
            {
                StoreAppendOutcome::Applied => Ok(PersistAppendOutcome::Applied),
                StoreAppendOutcome::Duplicate(existing) => Ok(PersistAppendOutcome::Duplicate(
                    Box::new(persisted_from_stored(existing)?),
                )),
                StoreAppendOutcome::RevisionConflict { .. } => {
                    Ok(PersistAppendOutcome::RevisionConflict)
                }
            }
        }
    }
}

pub(super) fn persisted_from_stored(stored: StoredRoom) -> Result<PersistedRoom, ApiError> {
    let persisted: PersistedRoom = serde_json::from_value(stored.snapshot)
        .map_err(|error| corrupt_snapshot(format!("snapshot JSON is invalid: {error}")))?;
    let stored_room_id = u128::from_str_radix(&stored.room_id, 16)
        .map(RoomId)
        .map_err(|_| corrupt_snapshot("stored room identifier is invalid"))?;
    if format_id(persisted.room_id.0) != stored.room_id
        || persisted.persistence_revision != stored.revision
        || persisted.room_id != stored_room_id
        || persisted
            .creation_command_id()
            .is_none_or(|command| format_id(command.0) != stored.creation_command_id)
    {
        return Err(corrupt_snapshot(
            "PostgreSQL room row and snapshot metadata disagree",
        ));
    }
    Ok(persisted)
}

pub(super) fn managed_from_persisted(persisted: PersistedRoom) -> Result<ManagedRoom, ApiError> {
    if persisted.side_effect_order.len() > SIDE_EFFECT_HISTORY_LIMIT
        || persisted
            .side_effect_order
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != persisted.side_effect_order.len()
    {
        return Err(corrupt_snapshot(
            "room side-effect history is unbounded or contains duplicates",
        ));
    }
    let creation_event = persisted
        .creation_event()
        .cloned()
        .ok_or_else(|| corrupt_snapshot("room snapshot has no creation event"))?;
    let room = if let Some(snapshot) = persisted.state {
        Room::restore_snapshot(snapshot)
    } else {
        Room::restore(
            persisted.room_id,
            persisted.mode,
            persisted.policy.clone(),
            &persisted.events,
        )
    }
    .map_err(|error| corrupt_snapshot(format!("room state cannot be restored: {error}")))?;
    let valid_creation = matches!(
        creation_event.kind,
        RoomEventKind::Created { host, mode }
            if creation_event.sequence == 1
                && mode == room.mode()
                && room.member_role(host) == Some(MemberRole::Host)
    );
    if room.id() != persisted.room_id
        || room.mode() != persisted.mode
        || room.policy() != &persisted.policy
        || room.sequence() != persisted.persistence_revision
        || !valid_creation
    {
        return Err(corrupt_snapshot(
            "room state and persisted metadata disagree",
        ));
    }
    let side_effect_order = VecDeque::from(persisted.side_effect_order);
    let side_effect_commands = side_effect_order.iter().copied().collect();
    Ok(ManagedRoom {
        room,
        creation_event,
        persistence_revision: persisted.persistence_revision,
        signals: VecDeque::new(),
        signal_cache_bytes: 0,
        next_signal_sequence: 1,
        side_effect_commands,
        side_effect_order,
    })
}

pub(super) fn load_postgres_rooms(stored_rooms: Vec<StoredRoom>) -> Result<LoadedRooms, ApiError> {
    let persisted = stored_rooms
        .into_iter()
        .map(persisted_from_stored)
        .collect::<Result<Vec<_>, _>>()?;
    loaded_rooms_from_persisted(persisted).map_err(|message| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "room_restore_failed",
        message,
    })
}

pub(super) fn load_rooms(directory: &Path) -> Result<LoadedRooms, String> {
    let entries = std::fs::read_dir(directory).map_err(|error| error.to_string())?;
    let mut latest = HashMap::<RoomId, PersistedRoom>::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                eprintln!("ignoring unreadable room snapshot directory entry: {error}");
                continue;
            }
        };
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = match read_bounded_room_snapshot(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "ignoring unreadable room snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let persisted = match serde_json::from_slice::<PersistedRoom>(&bytes) {
            Ok(persisted) => persisted,
            Err(error) => {
                eprintln!("ignoring invalid room snapshot {}: {error}", path.display());
                continue;
            }
        };
        let expected_name = snapshot_file_name(persisted.room_id, persisted.persistence_revision);
        if path.file_name().and_then(|value| value.to_str()) != Some(expected_name.as_str()) {
            eprintln!(
                "ignoring room snapshot with mismatched name {}",
                path.display()
            );
            continue;
        }
        if let Err(error) = managed_from_persisted(persisted.clone()) {
            eprintln!(
                "ignoring invalid room aggregate snapshot {}: {}",
                path.display(),
                error.message
            );
            continue;
        }
        let replace = latest
            .get(&persisted.room_id)
            .is_none_or(|current| persisted.persistence_revision > current.persistence_revision);
        if replace {
            latest.insert(persisted.room_id, persisted);
        }
    }
    loaded_rooms_from_persisted(latest.into_values())
}

pub(super) fn persist_managed_room(
    directory: &Path,
    room_id: RoomId,
    managed: &ManagedRoom,
) -> Result<(), ApiError> {
    let persisted = persisted_room(room_id, managed);
    let bytes = serde_json::to_vec(&persisted).map_err(internal_error)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ROOM_SNAPSHOT_BYTES {
        return Err(ApiError {
            status: StatusCode::INSUFFICIENT_STORAGE,
            code: "room_snapshot_too_large",
            message: "room snapshot exceeds 32 MiB".to_owned(),
        });
    }
    let path = directory.join(snapshot_file_name(room_id, managed.persistence_revision));
    if path.exists() {
        return match read_bounded_room_snapshot(&path) {
            Ok(existing) if existing == bytes => Ok(()),
            Ok(_) | Err(_) => Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "room_snapshot_revision_conflict",
                message: "room snapshot revision already contains different data".to_owned(),
            }),
        };
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| internal_error("room snapshot filename is invalid"))?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| state_io_error(&error))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        remove_temporary_snapshot(&temporary);
        return Err(state_io_error(&error));
    }
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let identical_target =
            read_bounded_room_snapshot(&path).is_ok_and(|existing| existing == bytes);
        remove_temporary_snapshot(&temporary);
        if !identical_target {
            return Err(state_io_error(&error));
        }
    }
    prune_room_snapshots(directory, room_id, managed.persistence_revision);
    Ok(())
}

fn read_bounded_room_snapshot(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("snapshot is not a regular file".to_owned());
    }
    if metadata.len() > MAX_ROOM_SNAPSHOT_BYTES {
        return Err("snapshot exceeds 32 MiB".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ROOM_SNAPSHOT_BYTES {
        return Err("snapshot exceeds 32 MiB".to_owned());
    }
    Ok(bytes)
}

fn remove_temporary_snapshot(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "failed to remove temporary room snapshot {}: {error}",
            path.display()
        );
    }
}

fn persisted_room(room_id: RoomId, managed: &ManagedRoom) -> PersistedRoom {
    PersistedRoom {
        room_id,
        mode: managed.room.mode(),
        policy: managed.room.policy().clone(),
        events: Vec::new(),
        state: Some(managed.room.snapshot()),
        creation_event: Some(managed.creation_event.clone()),
        persistence_revision: managed.persistence_revision,
        side_effect_order: managed.side_effect_order.iter().copied().collect(),
    }
}

fn stored_room(room_id: RoomId, managed: &ManagedRoom) -> Result<StoredRoom, ApiError> {
    let persisted = persisted_room(room_id, managed);
    let creation_command_id = persisted.creation_command_id().ok_or_else(|| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "missing_creation_event",
        message: "room event stream has no creation event".to_owned(),
    })?;
    Ok(StoredRoom {
        room_id: format_id(room_id.0),
        creation_command_id: format_id(creation_command_id.0),
        revision: managed.persistence_revision,
        snapshot: serde_json::to_value(persisted).map_err(internal_error)?,
        ended: managed.room.is_ended(),
    })
}

fn event_write(event: &RoomEvent) -> Result<EventWrite, ApiError> {
    Ok(EventWrite {
        sequence: event.sequence,
        command_id: format_id(event.command_id.0),
        event_type: room_event_type(&event.kind).to_owned(),
        event: serde_json::to_value(event).map_err(internal_error)?,
    })
}

const fn room_event_type(kind: &RoomEventKind) -> &'static str {
    match kind {
        RoomEventKind::Created { .. } => "room.created",
        RoomEventKind::MemberJoined { .. } => "room.member_joined",
        RoomEventKind::MemberLeft { .. } => "room.member_left",
        RoomEventKind::RoleChanged { .. } => "room.role_changed",
        RoomEventKind::PublishingStarted { .. } => "room.publishing_started",
        RoomEventKind::PublishingStopped { .. } => "room.publishing_stopped",
        RoomEventKind::ChatSent { .. } => "room.chat_sent",
        RoomEventKind::GiftRecorded { .. } => "room.gift_recorded",
        RoomEventKind::CustomDataSent { .. } => "room.custom_data_sent",
        RoomEventKind::Ended { .. } => "room.ended",
    }
}

fn gift_ledger_write(event: &RoomEvent) -> Option<GiftLedgerWrite> {
    let RoomEventKind::GiftRecorded {
        sender,
        receipt,
        total_value,
    } = &event.kind
    else {
        return None;
    };
    Some(GiftLedgerWrite {
        transaction_id: receipt.transaction_id.clone(),
        sender_id: format_id(sender.0),
        recipient_id: format_id(receipt.recipient.0),
        gift_id: receipt.gift_id.clone(),
        quantity: receipt.quantity,
        unit_value: receipt.unit_value,
        total_value: u128::from(*total_value),
        currency: receipt.currency.clone(),
    })
}

fn loaded_rooms_from_persisted(
    persisted_rooms: impl IntoIterator<Item = PersistedRoom>,
) -> Result<LoadedRooms, String> {
    let mut rooms = HashMap::new();
    let mut room_creations = HashMap::new();
    let mut event_channels = HashMap::new();
    for persisted in persisted_rooms {
        let room_id = persisted.room_id;
        let creation = persisted
            .creation_event()
            .ok_or_else(|| format!("room {} has an empty event stream", format_id(room_id.0)))?
            .command_id;
        let managed = managed_from_persisted(persisted).map_err(|error| error.message)?;
        if rooms.insert(room_id, managed).is_some() {
            return Err(format!("room {} is duplicated", format_id(room_id.0)));
        }
        if room_creations.insert(creation, room_id).is_some() {
            return Err(format!(
                "room creation command {} is duplicated",
                format_id(creation.0)
            ));
        }
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        event_channels.insert(room_id, sender);
    }
    Ok(LoadedRooms {
        rooms,
        room_creations,
        event_channels,
    })
}

fn prune_room_snapshots(directory: &Path, room_id: RoomId, current_revision: u64) {
    let prefix = format!("{}-", format_id(room_id.0));
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "failed to list room snapshots for {}: {error}",
                format_id(room_id.0)
            );
            return;
        }
    };
    let mut snapshots = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_str()?;
            let revision = file_name
                .strip_prefix(&prefix)?
                .strip_suffix(".json")?
                .parse::<u64>()
                .ok()?;
            (revision <= current_revision).then_some((revision, path))
        })
        .collect::<Vec<_>>();
    snapshots.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    for (_, path) in snapshots.into_iter().skip(2) {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to prune room snapshot {}: {error}", path.display());
        }
    }
}

fn snapshot_file_name(room_id: RoomId, revision: u64) -> String {
    format!("{}-{revision:020}.json", format_id(room_id.0))
}

fn format_id(value: u128) -> String {
    format!("{value:032x}")
}

fn corrupt_snapshot(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "corrupt_room_snapshot",
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};

    use fluvora_domain::{
        CommandId, CustomData, MemberRole, Room, RoomCommand, RoomEventKind, RoomId, RoomMode,
        RoomPolicy, UserId,
    };
    use tempfile::tempdir;

    use super::{
        ManagedRoom, PersistedRoom, SIDE_EFFECT_HISTORY_LIMIT, load_rooms,
        loaded_rooms_from_persisted, managed_from_persisted, persist_managed_room, persisted_room,
        snapshot_file_name,
    };

    fn managed_room(room_id: RoomId, command_id: CommandId) -> ManagedRoom {
        let (room, event) = Room::create(
            room_id,
            RoomMode::Live,
            UserId(10),
            RoomPolicy::default(),
            command_id,
            100,
        );
        ManagedRoom {
            room,
            creation_event: event,
            persistence_revision: 1,
            signals: VecDeque::new(),
            signal_cache_bytes: 0,
            next_signal_sequence: 1,
            side_effect_commands: HashSet::new(),
            side_effect_order: VecDeque::new(),
        }
    }

    #[test]
    fn restores_the_previous_valid_snapshot_and_ignores_forged_names() {
        let directory = tempdir().expect("temporary state directory");
        let room_id = RoomId(1);
        let mut managed = managed_room(room_id, CommandId(1));
        persist_managed_room(directory.path(), room_id, &managed).expect("persist snapshot");
        for revision in 2..=3 {
            managed
                .room
                .execute(
                    CommandId(u128::from(revision)),
                    revision,
                    RoomCommand::SendChat {
                        user: UserId(10),
                        message_id: u128::from(revision),
                        text: "snapshot advance".to_owned(),
                    },
                )
                .expect("advance room state");
            managed.persistence_revision = revision;
            persist_managed_room(directory.path(), room_id, &managed).expect("persist snapshot");
        }
        let retained = std::fs::read_dir(directory.path())
            .expect("list snapshots")
            .count();
        assert_eq!(retained, 2);

        std::fs::write(
            directory.path().join(snapshot_file_name(room_id, 3)),
            b"not-json",
        )
        .expect("corrupt newest snapshot");
        let mut forged = persisted_room(room_id, &managed);
        forged.persistence_revision = 999;
        std::fs::write(
            directory.path().join("forged.json"),
            serde_json::to_vec(&forged).expect("serialize forged snapshot"),
        )
        .expect("write forged snapshot");
        let mut invalid_aggregate = persisted_room(room_id, &managed);
        invalid_aggregate.persistence_revision = 4;
        invalid_aggregate.events.clear();
        invalid_aggregate.state = None;
        invalid_aggregate.creation_event = None;
        std::fs::write(
            directory.path().join(snapshot_file_name(room_id, 4)),
            serde_json::to_vec(&invalid_aggregate).expect("serialize invalid aggregate"),
        )
        .expect("write invalid aggregate");

        let loaded = load_rooms(directory.path()).expect("load fallback snapshot");
        assert_eq!(loaded.rooms.len(), 1);
        assert_eq!(loaded.rooms[&room_id].persistence_revision, 2);
    }

    #[test]
    fn persists_revisions_atomically_and_retries_idempotently() {
        let directory = tempdir().expect("temporary state directory");
        let room_id = RoomId(5);
        let managed = managed_room(room_id, CommandId(10));
        persist_managed_room(directory.path(), room_id, &managed).expect("persist snapshot");
        persist_managed_room(directory.path(), room_id, &managed).expect("idempotent retry");

        let mut conflicting = managed;
        conflicting.side_effect_commands.insert(CommandId(11));
        conflicting.side_effect_order.push_back(CommandId(11));
        let error = persist_managed_room(directory.path(), room_id, &conflicting)
            .expect_err("conflicting revision");
        assert_eq!(error.code, "room_snapshot_revision_conflict");
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("list snapshots")
                .count(),
            1
        );
    }

    #[test]
    fn compacts_large_event_history_and_reads_legacy_snapshots() {
        let room_id = RoomId(7);
        let mut managed = managed_room(room_id, CommandId(20));
        let mut legacy = persisted_room(room_id, &managed);
        legacy.events = vec![managed.creation_event.clone()];
        legacy.state = None;
        legacy.creation_event = None;
        let restored = managed_from_persisted(legacy).expect("restore legacy event snapshot");
        assert_eq!(restored.room.sequence(), 1);

        for sequence in 2..=128_u64 {
            managed
                .room
                .execute(
                    CommandId(u128::from(sequence) + 1_000),
                    sequence,
                    RoomCommand::SendCustomData {
                        user: UserId(10),
                        data: CustomData {
                            namespace: "com.example.snapshot".to_owned(),
                            schema_version: 1,
                            payload: vec![0; 60 * 1_024],
                        },
                    },
                )
                .expect("append large transient event");
            managed.persistence_revision = managed.room.sequence();
        }
        let compact = persisted_room(room_id, &managed);
        assert!(compact.events.is_empty());
        assert!(compact.state.is_some());
        assert!(serde_json::to_vec(&compact).expect("snapshot JSON").len() < 32 * 1_024);
        let snapshot_room = Room::restore_snapshot(compact.state.clone().expect("state"))
            .expect("valid compact state");
        assert_eq!(snapshot_room.id(), compact.room_id);
        assert_eq!(snapshot_room.mode(), compact.mode);
        assert_eq!(snapshot_room.policy(), &compact.policy);
        assert_eq!(snapshot_room.sequence(), compact.persistence_revision);
        assert!(matches!(
            compact.creation_event().map(|event| &event.kind),
            Some(RoomEventKind::Created { host, mode })
                if *mode == snapshot_room.mode()
                    && snapshot_room.member_role(*host) == Some(MemberRole::Host)
        ));
        assert_eq!(
            managed_from_persisted(compact)
                .expect("restore compact snapshot")
                .room
                .sequence(),
            128
        );
    }

    #[test]
    fn rejects_duplicate_creation_commands_and_unbounded_side_effect_history() {
        let first = persisted_room(RoomId(1), &managed_room(RoomId(1), CommandId(7)));
        let second = persisted_room(RoomId(2), &managed_room(RoomId(2), CommandId(7)));
        assert!(loaded_rooms_from_persisted([first, second]).is_err());

        let mut persisted = persisted_room(RoomId(3), &managed_room(RoomId(3), CommandId(8)));
        persisted.side_effect_order = (0..=SIDE_EFFECT_HISTORY_LIMIT)
            .map(|value| CommandId(value as u128))
            .collect();
        let error = managed_from_persisted(persisted).expect_err("unbounded history");
        assert_eq!(error.code, "corrupt_room_snapshot");
    }

    #[test]
    fn rejects_duplicate_side_effect_commands() {
        let mut persisted: PersistedRoom =
            persisted_room(RoomId(4), &managed_room(RoomId(4), CommandId(9)));
        persisted.side_effect_order = vec![CommandId(1), CommandId(1)];
        assert!(managed_from_persisted(persisted).is_err());
    }
}
