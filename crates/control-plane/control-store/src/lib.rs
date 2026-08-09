//! Transactional `PostgreSQL` control-plane persistence for Fluvora.
//!
//! The store owns room snapshot compare-and-swap, durable event/outbox writes, global creation
//! idempotency, gift-ledger uniqueness, leases, and media-node placement records. Callers keep
//! protocol state in their own bounded caches, but `PostgreSQL` remains the source of truth.

use std::fmt::Write as _;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Executor as _, PgPool, Postgres, Transaction};

const MIGRATION_LOCK_ID: i64 = 0x464c_5556_4f52_4101;
const SIGNAL_BACKLOG_MESSAGES: u32 = 128;
const MAX_ROOM_SNAPSHOT_BYTES: usize = 32 * 1_024 * 1_024;
const MAX_ROOM_EVENT_BYTES: usize = 1_024 * 1_024;
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "control_plane",
        include_str!("../../../../migrations/0001_control_plane.sql"),
    ),
    (
        2,
        "media_node_ice_candidate",
        include_str!("../../../../migrations/0002_media_node_ice_candidate.sql"),
    ),
    (
        3,
        "durable_room_signals",
        include_str!("../../../../migrations/0003_durable_room_signals.sql"),
    ),
    (
        4,
        "service_node_scheduling",
        include_str!("../../../../migrations/0004_service_node_scheduling.sql"),
    ),
    (
        5,
        "token_revocations",
        include_str!("../../../../migrations/0005_token_revocations.sql"),
    ),
];

/// A durable room snapshot returned from `PostgreSQL`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRoom {
    /// Lowercase 128-bit hexadecimal room identifier.
    pub room_id: String,
    /// Lowercase 128-bit hexadecimal creation idempotency key.
    pub creation_command_id: String,
    /// Monotonic compare-and-swap revision.
    pub revision: u64,
    /// Versioned application snapshot.
    pub snapshot: Value,
    /// Whether the room is terminal.
    pub ended: bool,
}

/// One event to atomically append with a room snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventWrite {
    /// Monotonic room event sequence.
    pub sequence: u64,
    /// Lowercase 128-bit hexadecimal command idempotency key.
    pub command_id: String,
    /// Stable event type used by consumers.
    pub event_type: String,
    /// Versioned event JSON.
    pub event: Value,
}

/// Optional immutable gift-ledger entry written in the room transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GiftLedgerWrite {
    /// Payment-provider transaction identifier.
    pub transaction_id: String,
    /// Lowercase sender identifier.
    pub sender_id: String,
    /// Lowercase recipient identifier.
    pub recipient_id: String,
    /// Catalog gift identifier.
    pub gift_id: String,
    /// Positive gift quantity.
    pub quantity: u32,
    /// Value per gift in the smallest currency unit.
    pub unit_value: u64,
    /// Checked aggregate value in the smallest currency unit.
    pub total_value: u128,
    /// Uppercase three-letter currency.
    pub currency: String,
}

/// Outcome of globally idempotent room creation.
#[derive(Debug, Clone, PartialEq)]
pub enum CreateRoomOutcome {
    /// This call inserted the room and creation event.
    Created,
    /// The same creation command already produced this durable room.
    Duplicate(StoredRoom),
}

/// Outcome of an optimistic room update.
#[derive(Debug, Clone, PartialEq)]
pub enum AppendOutcome {
    /// Snapshot, event, optional ledger, and outbox were committed.
    Applied,
    /// The command was already applied; the current durable snapshot is returned.
    Duplicate(StoredRoom),
    /// Another writer committed first.
    RevisionConflict {
        /// Revision currently stored by `PostgreSQL`.
        actual_revision: u64,
    },
}

/// Durable service lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    /// Logical resource kind, such as `worker_job`.
    pub resource_kind: String,
    /// Stable resource identifier.
    pub resource_id: String,
    /// Current owner instance identifier.
    pub owner_id: String,
    /// Fencing generation incremented on ownership change.
    pub generation: u64,
    /// Lease metadata.
    pub metadata: Value,
}

/// One leased transactional-outbox message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxMessage {
    /// Monotonic database identifier.
    pub id: i64,
    /// Aggregate category.
    pub aggregate_type: String,
    /// Aggregate identifier.
    pub aggregate_id: String,
    /// Aggregate-local sequence.
    pub aggregate_sequence: u64,
    /// Stable event type.
    pub event_type: String,
    /// Versioned event payload.
    pub payload: Value,
    /// Delivery attempt number, starting at one.
    pub attempts: u32,
}

/// One durable P2P signaling record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredSignal {
    /// Room owning the signal.
    pub room_id: String,
    /// Room-local monotonic signaling sequence.
    pub sequence: u64,
    /// Client or server idempotency key.
    pub command_id: String,
    /// Sender user identifier.
    pub from_id: String,
    /// Optional target user; absent means broadcast.
    pub to_id: Option<String>,
    /// Stable signaling kind.
    pub kind: String,
    /// Signaling body.
    pub payload: Value,
    /// Server timestamp in Unix milliseconds.
    pub timestamp_millis: u64,
}

/// Durable signaling replay page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalPage {
    /// Recipient-filtered signaling records.
    pub signals: Vec<StoredSignal>,
    /// Latest sequence allocated in the room, including signals for other recipients.
    pub latest_sequence: u64,
}

/// Capacity heartbeat used for durable media-node discovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaNodeHeartbeat {
    /// Stable media-node instance identifier.
    pub node_id: String,
    /// Placement region.
    pub region: String,
    /// Internal HTTP control endpoint.
    pub endpoint: String,
    /// Node-specific SDP ICE candidate line without the `a=candidate:` prefix.
    pub ice_candidate: Option<String>,
    /// Whether dependency and protocol checks pass.
    pub healthy: bool,
    /// Whether new rooms must be rejected.
    pub draining: bool,
    /// Rooms currently reported by the node.
    pub rooms_used: u64,
    /// Hard room capacity.
    pub rooms_limit: u64,
    /// Sessions currently reported by the node.
    pub sessions_used: u64,
    /// Hard session capacity.
    pub sessions_limit: u64,
    /// Publisher tracks currently routed by the node.
    pub publisher_tracks: u64,
    /// Extensible bounded metadata.
    pub metadata: Value,
}

/// Generic schedulable internal service heartbeat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceNodeHeartbeat {
    /// Stable instance identifier.
    pub node_id: String,
    /// Stable service kind, such as `media_worker`.
    pub service_kind: String,
    /// Placement region.
    pub region: String,
    /// Internal HTTP control endpoint.
    pub endpoint: String,
    /// Whether the process can serve work.
    pub healthy: bool,
    /// Whether new work must be avoided.
    pub draining: bool,
    /// Reported running or queued jobs.
    pub jobs_used: u64,
    /// Maximum concurrent jobs.
    pub jobs_limit: u64,
    /// Versioned service metadata.
    pub metadata: Value,
}

/// Fenced service resource placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServicePlacement {
    /// Logical resource kind.
    pub resource_kind: String,
    /// Stable resource identifier.
    pub resource_id: String,
    /// Selected node.
    pub node_id: String,
    /// Selected internal endpoint.
    pub endpoint: String,
    /// Fencing generation incremented on reassignment.
    pub generation: u64,
}

/// Durable, fenced room-to-media-node assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPlacement {
    /// Lowercase room identifier.
    pub room_id: String,
    /// Selected media-node identifier.
    pub node_id: String,
    /// Internal HTTP control endpoint.
    pub endpoint: String,
    /// Node-specific SDP ICE candidate.
    pub ice_candidate: Option<String>,
    /// Generation incremented on reassignment.
    pub generation: u64,
}

/// PostgreSQL-backed control-plane repository.
#[derive(Debug, Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Connects with a bounded pool.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for an invalid URL, pool limit, or connection failure.
    pub async fn connect(database_url: &str, maximum_connections: u32) -> Result<Self, StoreError> {
        if !(1..=256).contains(&maximum_connections) {
            return Err(StoreError::InvalidPoolSize(maximum_connections));
        }
        let options: PgConnectOptions = database_url
            .parse()
            .map_err(|error: sqlx::Error| StoreError::Database(error.to_string()))?;
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(maximum_connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(StoreError::from)?;
        Ok(Self { pool })
    }

    /// Applies embedded, checksummed, strictly ordered migrations under an advisory lock.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if migration state is inconsistent or `PostgreSQL` rejects a step.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MIGRATION_LOCK_ID)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
        transaction
            .execute(
                "CREATE TABLE IF NOT EXISTS fluvora_schema_migrations (\
                 version bigint PRIMARY KEY, name text NOT NULL UNIQUE, checksum char(64) NOT NULL,\
                 applied_at timestamptz NOT NULL DEFAULT clock_timestamp())",
            )
            .await
            .map_err(StoreError::from)?;

        for &(version, name, sql) in MIGRATIONS {
            apply_migration(&mut transaction, version, name, sql).await?;
        }
        transaction.commit().await.map_err(StoreError::from)
    }

    /// Executes a one-round-trip database health check.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if no healthy connection is available.
    pub async fn healthcheck(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(StoreError::from)
    }

    /// Loads all durable room snapshots in stable identifier order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for database or corrupt numeric data.
    pub async fn load_rooms(&self) -> Result<Vec<StoredRoom>, StoreError> {
        let rows = sqlx::query_as::<_, RoomRow>(
            "SELECT room_id, creation_command_id, revision, snapshot, ended \
             FROM fluvora_rooms ORDER BY room_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;
        rows.into_iter().map(StoredRoom::try_from).collect()
    }

    /// Loads one room by identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid identifiers, database errors, or corrupt revisions.
    pub async fn load_room(&self, room_id: &str) -> Result<Option<StoredRoom>, StoreError> {
        validate_hex_id(room_id)?;
        sqlx::query_as::<_, RoomRow>(
            "SELECT room_id, creation_command_id, revision, snapshot, ended \
             FROM fluvora_rooms WHERE room_id = $1",
        )
        .bind(room_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?
        .map(StoredRoom::try_from)
        .transpose()
    }

    /// Atomically creates a room, its first event, and an outbox record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid data, identifier collision, or database failure.
    pub async fn create_room(
        &self,
        room: &StoredRoom,
        event: &EventWrite,
    ) -> Result<CreateRoomOutcome, StoreError> {
        validate_room_write(room, event)?;
        if room.revision != 1 || event.sequence != 1 {
            return Err(StoreError::InvalidInitialRevision);
        }
        let revision = to_i64(room.revision, "revision")?;
        let sequence = to_i64(event.sequence, "event sequence")?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let inserted = sqlx::query(
            "INSERT INTO fluvora_rooms \
             (room_id, creation_command_id, revision, snapshot, ended) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(&room.room_id)
        .bind(&room.creation_command_id)
        .bind(revision)
        .bind(&room.snapshot)
        .bind(room.ended)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        if inserted == 0 {
            let existing = load_room_by_creation(&mut transaction, &room.creation_command_id)
                .await?
                .ok_or(StoreError::IdentifierCollision)?;
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(CreateRoomOutcome::Duplicate(existing));
        }
        insert_event_and_outbox(&mut transaction, &room.room_id, event, sequence).await?;
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(CreateRoomOutcome::Created)
    }

    /// Atomically compare-and-swaps a snapshot with one event, outbox item, and optional gift.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid data, database failure, or gift-ledger conflict.
    pub async fn append_room_event(
        &self,
        room: &StoredRoom,
        expected_revision: u64,
        event: &EventWrite,
        gift: Option<&GiftLedgerWrite>,
    ) -> Result<AppendOutcome, StoreError> {
        validate_room_write(room, event)?;
        if room.revision != expected_revision.saturating_add(1) {
            return Err(StoreError::InvalidRevisionTransition {
                expected: expected_revision.saturating_add(1),
                actual: room.revision,
            });
        }
        if let Some(gift) = gift {
            validate_gift(gift)?;
        }
        let expected = to_i64(expected_revision, "expected revision")?;
        let revision = to_i64(room.revision, "revision")?;
        let sequence = to_i64(event.sequence, "event sequence")?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let locked = sqlx::query_as::<_, RoomRow>(
            "SELECT room_id, creation_command_id, revision, snapshot, ended \
             FROM fluvora_rooms WHERE room_id = $1 FOR UPDATE",
        )
        .bind(&room.room_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .ok_or(StoreError::RoomNotFound)?;
        let stored = StoredRoom::try_from(locked)?;

        if event_exists(&mut transaction, &room.room_id, &event.command_id).await? {
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(AppendOutcome::Duplicate(stored));
        }
        if stored.revision != expected_revision {
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(AppendOutcome::RevisionConflict {
                actual_revision: stored.revision,
            });
        }

        let updated = sqlx::query(
            "UPDATE fluvora_rooms SET revision = $2, snapshot = $3, ended = $4, \
             updated_at = clock_timestamp() WHERE room_id = $1 AND revision = $5",
        )
        .bind(&room.room_id)
        .bind(revision)
        .bind(&room.snapshot)
        .bind(room.ended)
        .bind(expected)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        if updated != 1 {
            return Err(StoreError::LostDatabaseLock);
        }
        insert_event_and_outbox(&mut transaction, &room.room_id, event, sequence).await?;
        if let Some(gift) = gift {
            insert_gift(&mut transaction, &room.room_id, sequence, gift).await?;
        }
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(AppendOutcome::Applied)
    }

    /// Records a side-effect command exactly once across API replicas.
    ///
    /// Returns `true` only for the first caller.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid identifiers or database failure.
    pub async fn mark_side_effect(
        &self,
        room_id: &str,
        command_id: &str,
    ) -> Result<bool, StoreError> {
        validate_hex_id(room_id)?;
        validate_hex_id(command_id)?;
        let affected = sqlx::query(
            "INSERT INTO fluvora_side_effects (room_id, command_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(room_id)
        .bind(command_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Returns whether a cross-replica side effect has already completed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid identifiers or database failure.
    pub async fn side_effect_exists(
        &self,
        room_id: &str,
        command_id: &str,
    ) -> Result<bool, StoreError> {
        validate_hex_id(room_id)?;
        validate_hex_id(command_id)?;
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM fluvora_side_effects \
             WHERE room_id = $1 AND command_id = $2)",
        )
        .bind(room_id)
        .bind(command_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)
    }

    /// Acquires or renews a fenced lease.
    ///
    /// A different owner can take over only after expiration; generation increases on takeover.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid fields or database failure.
    pub async fn acquire_lease(
        &self,
        resource_kind: &str,
        resource_id: &str,
        owner_id: &str,
        ttl: Duration,
        metadata: &Value,
    ) -> Result<Option<Lease>, StoreError> {
        validate_bounded_text(resource_kind, "resource kind", 64)?;
        validate_bounded_text(resource_id, "resource id", 256)?;
        validate_bounded_text(owner_id, "owner id", 256)?;
        let ttl_millis = ttl.as_millis();
        if !(1_000..=300_000).contains(&ttl_millis) {
            return Err(StoreError::InvalidLeaseTtl(ttl_millis));
        }
        let ttl_millis =
            i64::try_from(ttl_millis).map_err(|_| StoreError::InvalidLeaseTtl(ttl_millis))?;
        let row = sqlx::query_as::<_, LeaseRow>(
            "INSERT INTO fluvora_service_leases \
             (resource_kind, resource_id, owner_id, lease_until, metadata) \
             VALUES ($1, $2, $3, clock_timestamp() + ($4 * interval '1 millisecond'), $5) \
             ON CONFLICT (resource_kind, resource_id) DO UPDATE SET \
               owner_id = EXCLUDED.owner_id, \
               generation = CASE WHEN fluvora_service_leases.owner_id = EXCLUDED.owner_id \
                                 THEN fluvora_service_leases.generation \
                                 ELSE fluvora_service_leases.generation + 1 END, \
               lease_until = EXCLUDED.lease_until, metadata = EXCLUDED.metadata, \
               updated_at = clock_timestamp() \
             WHERE fluvora_service_leases.owner_id = EXCLUDED.owner_id \
                OR fluvora_service_leases.lease_until <= clock_timestamp() \
             RETURNING resource_kind, resource_id, owner_id, generation, metadata",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .bind(owner_id)
        .bind(ttl_millis)
        .bind(metadata)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;
        row.map(Lease::try_from).transpose()
    }

    /// Releases a lease only when owner and fencing generation still match.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid fields or database failure.
    pub async fn release_lease(&self, lease: &Lease) -> Result<bool, StoreError> {
        validate_bounded_text(&lease.resource_kind, "resource kind", 64)?;
        validate_bounded_text(&lease.resource_id, "resource id", 256)?;
        validate_bounded_text(&lease.owner_id, "owner id", 256)?;
        let generation = to_i64(lease.generation, "lease generation")?;
        let affected = sqlx::query(
            "DELETE FROM fluvora_service_leases WHERE resource_kind = $1 AND resource_id = $2 \
             AND owner_id = $3 AND generation = $4",
        )
        .bind(&lease.resource_kind)
        .bind(&lease.resource_id)
        .bind(&lease.owner_id)
        .bind(generation)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Leases pending outbox messages with `FOR UPDATE SKIP LOCKED`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid owner, bounds, or database failure.
    pub async fn claim_outbox(
        &self,
        owner_id: &str,
        maximum_messages: u32,
        lease_ttl: Duration,
    ) -> Result<Vec<OutboxMessage>, StoreError> {
        validate_bounded_text(owner_id, "outbox owner", 256)?;
        if !(1..=1_000).contains(&maximum_messages) {
            return Err(StoreError::InvalidBatchSize(maximum_messages));
        }
        let ttl_millis = lease_ttl.as_millis();
        if !(1_000..=300_000).contains(&ttl_millis) {
            return Err(StoreError::InvalidLeaseTtl(ttl_millis));
        }
        let ttl_millis =
            i64::try_from(ttl_millis).map_err(|_| StoreError::InvalidLeaseTtl(ttl_millis))?;
        let limit = i64::from(maximum_messages);
        let rows = sqlx::query_as::<_, OutboxRow>(
            "WITH selected AS (\
               SELECT id FROM fluvora_outbox \
               WHERE delivered_at IS NULL AND available_at <= clock_timestamp() \
                 AND (lease_until IS NULL OR lease_until <= clock_timestamp()) \
               ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $1\
             ) \
             UPDATE fluvora_outbox AS item SET lease_owner = $2, \
               lease_until = clock_timestamp() + ($3 * interval '1 millisecond'), \
               attempts = item.attempts + 1 \
             FROM selected WHERE item.id = selected.id \
             RETURNING item.id, item.aggregate_type, item.aggregate_id, \
               item.aggregate_sequence, item.event_type, item.payload, item.attempts",
        )
        .bind(limit)
        .bind(owner_id)
        .bind(ttl_millis)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;
        rows.into_iter().map(OutboxMessage::try_from).collect()
    }

    /// Marks a leased outbox message delivered.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when ownership is invalid or the database rejects the update.
    pub async fn acknowledge_outbox(
        &self,
        owner_id: &str,
        message_id: i64,
    ) -> Result<bool, StoreError> {
        validate_bounded_text(owner_id, "outbox owner", 256)?;
        if message_id <= 0 {
            return Err(StoreError::InvalidOutboxId(message_id));
        }
        let affected = sqlx::query(
            "UPDATE fluvora_outbox SET delivered_at = clock_timestamp(), lease_owner = NULL, \
             lease_until = NULL, last_error = NULL \
             WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL",
        )
        .bind(message_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Deletes one bounded batch of delivered outbox rows older than the retention window.
    ///
    /// Pending and leased rows are never selected. Repeated calls allow operators to drain a large
    /// historical backlog without one unbounded transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for an unsafe retention or batch size, or a database failure.
    pub async fn prune_delivered_outbox(
        &self,
        retention: Duration,
        maximum_messages: u32,
    ) -> Result<u64, StoreError> {
        let retention_millis = retention.as_millis();
        if !(3_600_000..=31_536_000_000).contains(&retention_millis) {
            return Err(StoreError::InvalidOutboxRetention(retention_millis));
        }
        if !(1..=10_000).contains(&maximum_messages) {
            return Err(StoreError::InvalidBatchSize(maximum_messages));
        }
        let retention_millis = i64::try_from(retention_millis)
            .map_err(|_| StoreError::InvalidOutboxRetention(retention_millis))?;
        let affected = sqlx::query(
            "WITH expired AS (\
               SELECT id FROM fluvora_outbox \
               WHERE delivered_at < clock_timestamp() - ($1 * interval '1 millisecond') \
               ORDER BY id LIMIT $2\
             ) \
             DELETE FROM fluvora_outbox AS item USING expired \
             WHERE item.id = expired.id",
        )
        .bind(retention_millis)
        .bind(i64::from(maximum_messages))
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        Ok(affected)
    }

    /// Releases a failed outbox message with bounded retry delay and error detail.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid input or database failure.
    pub async fn retry_outbox(
        &self,
        owner_id: &str,
        message_id: i64,
        delay: Duration,
        error: &str,
    ) -> Result<bool, StoreError> {
        validate_bounded_text(owner_id, "outbox owner", 256)?;
        validate_bounded_text(error, "outbox error", 2_048)?;
        if message_id <= 0 {
            return Err(StoreError::InvalidOutboxId(message_id));
        }
        let delay_millis = delay.as_millis();
        if delay_millis > 3_600_000 {
            return Err(StoreError::InvalidRetryDelay(delay_millis));
        }
        let delay_millis =
            i64::try_from(delay_millis).map_err(|_| StoreError::InvalidRetryDelay(delay_millis))?;
        let affected = sqlx::query(
            "UPDATE fluvora_outbox SET available_at = \
               clock_timestamp() + ($3 * interval '1 millisecond'), \
             lease_owner = NULL, lease_until = NULL, last_error = $4 \
             WHERE id = $1 AND lease_owner = $2 AND delivered_at IS NULL",
        )
        .bind(message_id)
        .bind(owner_id)
        .bind(delay_millis)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Atomically allocates a room-local sequence, writes a signal, and appends its outbox event.
    ///
    /// Reusing `command_id` in the same room returns the original signal.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid fields, a missing room, or database failure.
    pub async fn append_room_signal(
        &self,
        signal: &StoredSignal,
    ) -> Result<StoredSignal, StoreError> {
        validate_signal(signal)?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let room_exists = sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM fluvora_rooms WHERE room_id = $1 FOR UPDATE",
        )
        .bind(&signal.room_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .is_some();
        if !room_exists {
            return Err(StoreError::RoomNotFound);
        }
        if let Some(existing) = sqlx::query_as::<_, SignalRow>(
            "SELECT room_id, sequence, command_id, from_id, to_id, kind, payload, \
             timestamp_millis FROM fluvora_room_signals \
             WHERE room_id = $1 AND command_id = $2",
        )
        .bind(&signal.room_id)
        .bind(&signal.command_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        {
            transaction.commit().await.map_err(StoreError::from)?;
            return StoredSignal::try_from(existing);
        }
        let sequence = sqlx::query_scalar::<_, i64>(
            "UPDATE fluvora_rooms SET signal_sequence = signal_sequence + 1 \
             WHERE room_id = $1 RETURNING signal_sequence",
        )
        .bind(&signal.room_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .ok_or(StoreError::RoomNotFound)?;
        let stored = StoredSignal {
            sequence: to_u64(sequence, "signal sequence")?,
            ..signal.clone()
        };
        sqlx::query(
            "INSERT INTO fluvora_room_signals \
             (room_id, sequence, command_id, from_id, to_id, kind, payload, timestamp_millis) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&stored.room_id)
        .bind(sequence)
        .bind(&stored.command_id)
        .bind(&stored.from_id)
        .bind(&stored.to_id)
        .bind(&stored.kind)
        .bind(&stored.payload)
        .bind(to_i64(stored.timestamp_millis, "signal timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        let outbox_payload = serde_json::to_value(&stored)
            .map_err(|error| StoreError::InvalidJson(error.to_string()))?;
        sqlx::query(
            "INSERT INTO fluvora_outbox \
             (aggregate_type, aggregate_id, aggregate_sequence, event_type, payload) \
             VALUES ('room_signal', $1, $2, 'signal.created', $3)",
        )
        .bind(&stored.room_id)
        .bind(sequence)
        .bind(outbox_payload)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        sqlx::query(
            "DELETE FROM fluvora_room_signals \
             WHERE room_id = $1 AND sequence <= $2 - $3",
        )
        .bind(&stored.room_id)
        .bind(sequence)
        .bind(i64::from(SIGNAL_BACKLOG_MESSAGES))
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(stored)
    }

    /// Loads a bounded recipient-filtered signaling replay page.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid fields, bounds, a missing room, or database failure.
    pub async fn load_room_signal_page(
        &self,
        room_id: &str,
        after: u64,
        maximum_messages: u32,
        recipient_id: &str,
    ) -> Result<SignalPage, StoreError> {
        validate_hex_id(room_id)?;
        validate_hex_id(recipient_id)?;
        if !(1..=SIGNAL_BACKLOG_MESSAGES).contains(&maximum_messages) {
            return Err(StoreError::InvalidBatchSize(maximum_messages));
        }
        let after = to_i64(after, "signal cursor")?;
        let room_sequence = sqlx::query_scalar::<_, i64>(
            "SELECT signal_sequence FROM fluvora_rooms WHERE room_id = $1",
        )
        .bind(room_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?
        .ok_or(StoreError::RoomNotFound)?;
        let rows = sqlx::query_as::<_, SignalRow>(
            "SELECT room_id, sequence, command_id, from_id, to_id, kind, payload, \
             timestamp_millis FROM fluvora_room_signals \
             WHERE room_id = $1 AND sequence > $2 AND (to_id IS NULL OR to_id = $3) \
             ORDER BY sequence LIMIT $4",
        )
        .bind(room_id)
        .bind(after)
        .bind(recipient_id)
        .bind(i64::from(maximum_messages))
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;
        Ok(SignalPage {
            signals: rows
                .into_iter()
                .map(StoredSignal::try_from)
                .collect::<Result<_, _>>()?,
            latest_sequence: to_u64(room_sequence, "signal sequence")?,
        })
    }

    /// Inserts or refreshes a media-node capacity heartbeat.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid capacity, endpoint, metadata, or database failure.
    pub async fn upsert_media_node(
        &self,
        heartbeat: &MediaNodeHeartbeat,
    ) -> Result<(), StoreError> {
        validate_media_node(heartbeat)?;
        sqlx::query(
            "INSERT INTO fluvora_media_nodes \
             (node_id, region, endpoint, ice_candidate, healthy, draining, rooms_used, rooms_limit, \
              sessions_used, sessions_limit, publisher_tracks, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (node_id) DO UPDATE SET region = EXCLUDED.region, \
               endpoint = EXCLUDED.endpoint, ice_candidate = EXCLUDED.ice_candidate, \
               healthy = EXCLUDED.healthy, \
               draining = EXCLUDED.draining, rooms_used = EXCLUDED.rooms_used, \
               rooms_limit = EXCLUDED.rooms_limit, sessions_used = EXCLUDED.sessions_used, \
               sessions_limit = EXCLUDED.sessions_limit, \
               publisher_tracks = EXCLUDED.publisher_tracks, \
               heartbeat_at = clock_timestamp(), metadata = EXCLUDED.metadata",
        )
        .bind(&heartbeat.node_id)
        .bind(&heartbeat.region)
        .bind(&heartbeat.endpoint)
        .bind(&heartbeat.ice_candidate)
        .bind(heartbeat.healthy)
        .bind(heartbeat.draining)
        .bind(to_i64(heartbeat.rooms_used, "rooms used")?)
        .bind(to_i64(heartbeat.rooms_limit, "rooms limit")?)
        .bind(to_i64(heartbeat.sessions_used, "sessions used")?)
        .bind(to_i64(heartbeat.sessions_limit, "sessions limit")?)
        .bind(to_i64(heartbeat.publisher_tracks, "publisher tracks")?)
        .bind(&heartbeat.metadata)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(StoreError::from)
    }

    /// Returns an existing healthy room assignment or selects the least-loaded eligible node.
    ///
    /// The selected node row is locked so concurrent schedulers cannot overbook from the same
    /// placement snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid inputs, no capacity, or database failure.
    pub async fn place_room(
        &self,
        room_id: &str,
        region: &str,
        stale_after: Duration,
    ) -> Result<RoomPlacement, StoreError> {
        validate_hex_id(room_id)?;
        validate_bounded_text(region, "region", 128)?;
        let stale_millis = stale_after.as_millis();
        if !(1_000..=300_000).contains(&stale_millis) {
            return Err(StoreError::InvalidStaleWindow(stale_millis));
        }
        let stale_millis = i64::try_from(stale_millis)
            .map_err(|_| StoreError::InvalidStaleWindow(stale_millis))?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        if let Some(existing) =
            load_healthy_placement(&mut transaction, room_id, stale_millis).await?
        {
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(existing);
        }
        let node = select_media_node(&mut transaction, region, stale_millis)
            .await?
            .ok_or(StoreError::NoMediaNodeCapacity)?;
        let row = sqlx::query_as::<_, PlacementRow>(
            "INSERT INTO fluvora_room_placements (room_id, node_id) VALUES ($1, $2) \
             ON CONFLICT (room_id) DO UPDATE SET node_id = EXCLUDED.node_id, \
               generation = fluvora_room_placements.generation + 1, \
               updated_at = clock_timestamp() \
             RETURNING room_id, node_id, generation",
        )
        .bind(room_id)
        .bind(&node.node_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        transaction.commit().await.map_err(StoreError::from)?;
        RoomPlacement::from_row(row, node.endpoint, node.ice_candidate)
    }

    /// Removes a room assignment, allowing a later placement to choose a fresh node.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid identifiers or database failure.
    pub async fn remove_room_placement(&self, room_id: &str) -> Result<bool, StoreError> {
        validate_hex_id(room_id)?;
        let affected = sqlx::query("DELETE FROM fluvora_room_placements WHERE room_id = $1")
            .bind(room_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::from)?
            .rows_affected();
        Ok(affected == 1)
    }

    /// Inserts or refreshes a generic schedulable service heartbeat.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid capacity, endpoint, metadata, or database failure.
    pub async fn upsert_service_node(
        &self,
        heartbeat: &ServiceNodeHeartbeat,
    ) -> Result<(), StoreError> {
        validate_service_node(heartbeat)?;
        sqlx::query(
            "INSERT INTO fluvora_service_nodes \
             (node_id, service_kind, region, endpoint, healthy, draining, jobs_used, jobs_limit, \
              metadata) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (node_id) DO UPDATE SET service_kind = EXCLUDED.service_kind, \
               region = EXCLUDED.region, endpoint = EXCLUDED.endpoint, healthy = EXCLUDED.healthy, \
               draining = EXCLUDED.draining, jobs_used = EXCLUDED.jobs_used, \
               jobs_limit = EXCLUDED.jobs_limit, heartbeat_at = clock_timestamp(), \
               metadata = EXCLUDED.metadata",
        )
        .bind(&heartbeat.node_id)
        .bind(&heartbeat.service_kind)
        .bind(&heartbeat.region)
        .bind(&heartbeat.endpoint)
        .bind(heartbeat.healthy)
        .bind(heartbeat.draining)
        .bind(to_i64(heartbeat.jobs_used, "jobs used")?)
        .bind(to_i64(heartbeat.jobs_limit, "jobs limit")?)
        .bind(&heartbeat.metadata)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(StoreError::from)
    }

    /// Returns an existing healthy service placement or assigns the least-loaded eligible node.
    ///
    /// A transaction-scoped advisory lock serializes concurrent placement of the same resource.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid inputs, no capacity, or database failure.
    pub async fn place_service_resource(
        &self,
        resource_kind: &str,
        resource_id: &str,
        service_kind: &str,
        region: &str,
        stale_after: Duration,
    ) -> Result<ServicePlacement, StoreError> {
        validate_bounded_text(resource_kind, "resource kind", 64)?;
        validate_bounded_text(resource_id, "resource id", 256)?;
        validate_bounded_text(service_kind, "service kind", 64)?;
        validate_bounded_text(region, "region", 128)?;
        let stale_millis = checked_stale_millis(stale_after)?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let lock_key = format!("{resource_kind}:{resource_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
        let current = sqlx::query_as::<_, ServicePlacementRow>(
            "SELECT p.resource_kind, p.resource_id, p.node_id, p.generation, n.endpoint \
             FROM fluvora_service_resource_placements p \
             JOIN fluvora_service_nodes n ON n.node_id = p.node_id \
             WHERE p.resource_kind = $1 AND p.resource_id = $2",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if let Some(healthy) = sqlx::query_as::<_, ServicePlacementRow>(
            "SELECT p.resource_kind, p.resource_id, p.node_id, p.generation, n.endpoint \
             FROM fluvora_service_resource_placements p \
             JOIN fluvora_service_nodes n ON n.node_id = p.node_id \
             WHERE p.resource_kind = $1 AND p.resource_id = $2 AND n.service_kind = $3 \
               AND n.region = $4 AND n.healthy AND NOT n.draining \
               AND n.heartbeat_at >= clock_timestamp() - ($5 * interval '1 millisecond')",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .bind(service_kind)
        .bind(region)
        .bind(stale_millis)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        {
            transaction.commit().await.map_err(StoreError::from)?;
            return ServicePlacement::try_from(healthy);
        }
        let node = sqlx::query_as::<_, ServiceNodeRow>(
            "SELECT node_id, endpoint FROM fluvora_service_nodes \
             WHERE service_kind = $1 AND region = $2 AND healthy AND NOT draining \
               AND heartbeat_at >= clock_timestamp() - ($3 * interval '1 millisecond') \
               AND jobs_used < jobs_limit \
             ORDER BY (jobs_used::DOUBLE PRECISION / jobs_limit::DOUBLE PRECISION), \
               heartbeat_at DESC, node_id \
             FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .bind(service_kind)
        .bind(region)
        .bind(stale_millis)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .ok_or(StoreError::NoServiceNodeCapacity)?;
        let row = sqlx::query_as::<_, ServicePlacementRow>(
            "INSERT INTO fluvora_service_resource_placements \
             (resource_kind, resource_id, node_id) VALUES ($1, $2, $3) \
             ON CONFLICT (resource_kind, resource_id) DO UPDATE SET node_id = EXCLUDED.node_id, \
               generation = fluvora_service_resource_placements.generation + 1, \
               assigned_at = clock_timestamp() \
             RETURNING resource_kind, resource_id, node_id, generation, $4::TEXT AS endpoint",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .bind(&node.node_id)
        .bind(&node.endpoint)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        sqlx::query(
            "UPDATE fluvora_service_nodes SET jobs_used = LEAST(jobs_limit, jobs_used + 1) \
             WHERE node_id = $1",
        )
        .bind(&node.node_id)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if let Some(current) = current
            && current.node_id != node.node_id
        {
            sqlx::query(
                "UPDATE fluvora_service_nodes SET jobs_used = GREATEST(0, jobs_used - 1) \
                 WHERE node_id = $1",
            )
            .bind(current.node_id)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
        }
        transaction.commit().await.map_err(StoreError::from)?;
        ServicePlacement::try_from(row)
    }

    /// Revalidates a service placement and advances its fencing generation for a restart.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid inputs, no capacity, or database failure.
    pub async fn advance_service_placement(
        &self,
        resource_kind: &str,
        resource_id: &str,
        service_kind: &str,
        region: &str,
        stale_after: Duration,
    ) -> Result<ServicePlacement, StoreError> {
        let placement = self
            .place_service_resource(
                resource_kind,
                resource_id,
                service_kind,
                region,
                stale_after,
            )
            .await?;
        let generation = sqlx::query_scalar::<_, i64>(
            "UPDATE fluvora_service_resource_placements SET generation = generation + 1, \
             assigned_at = clock_timestamp() \
             WHERE resource_kind = $1 AND resource_id = $2 AND node_id = $3 \
             RETURNING generation",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .bind(&placement.node_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?
        .ok_or(StoreError::LostDatabaseLock)?;
        Ok(ServicePlacement {
            generation: to_u64(generation, "service placement generation")?,
            ..placement
        })
    }

    /// Removes a generic service resource placement and releases its reservation.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid fields or database failure.
    pub async fn remove_service_placement(
        &self,
        resource_kind: &str,
        resource_id: &str,
    ) -> Result<bool, StoreError> {
        validate_bounded_text(resource_kind, "resource kind", 64)?;
        validate_bounded_text(resource_id, "resource id", 256)?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let node_id = sqlx::query_scalar::<_, String>(
            "DELETE FROM fluvora_service_resource_placements \
             WHERE resource_kind = $1 AND resource_id = $2 RETURNING node_id",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if let Some(node_id) = &node_id {
            sqlx::query(
                "UPDATE fluvora_service_nodes SET jobs_used = GREATEST(0, jobs_used - 1) \
                 WHERE node_id = $1",
            )
            .bind(node_id)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
        }
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(node_id.is_some())
    }

    /// Removes a generic service placement only when its fencing generation still matches.
    ///
    /// This prevents cleanup from a superseded attempt from deleting a newer placement.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid fields, numeric overflow, or database failure.
    pub async fn remove_service_placement_generation(
        &self,
        resource_kind: &str,
        resource_id: &str,
        generation: u64,
    ) -> Result<bool, StoreError> {
        validate_bounded_text(resource_kind, "resource kind", 64)?;
        validate_bounded_text(resource_id, "resource id", 256)?;
        let generation = to_i64(generation, "service placement generation")?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let node_id = sqlx::query_scalar::<_, String>(
            "DELETE FROM fluvora_service_resource_placements \
             WHERE resource_kind = $1 AND resource_id = $2 AND generation = $3 \
             RETURNING node_id",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .bind(generation)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if let Some(node_id) = &node_id {
            sqlx::query(
                "UPDATE fluvora_service_nodes SET jobs_used = GREATEST(0, jobs_used - 1) \
                 WHERE node_id = $1",
            )
            .bind(node_id)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
        }
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(node_id.is_some())
    }

    /// Persists or extends an access-token revocation.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for malformed identifiers, reason, timestamp, or database failure.
    pub async fn revoke_access_token(
        &self,
        subject: &str,
        nonce: u64,
        expires_at_millis: u64,
        reason: &str,
    ) -> Result<(), StoreError> {
        validate_hex_id(subject)?;
        validate_bounded_text(reason, "revocation reason", 512)?;
        let nonce = nonce.to_string();
        let expires_at_millis = to_i64(expires_at_millis, "token expiration")?;
        sqlx::query(
            "INSERT INTO fluvora_token_revocations \
             (subject_id, token_nonce, expires_at, reason) \
             VALUES ($1, $2::NUMERIC, to_timestamp($3::DOUBLE PRECISION / 1000.0), $4) \
             ON CONFLICT (subject_id, token_nonce) DO UPDATE SET \
               expires_at = GREATEST(fluvora_token_revocations.expires_at, EXCLUDED.expires_at), \
               reason = EXCLUDED.reason, revoked_at = clock_timestamp()",
        )
        .bind(subject)
        .bind(nonce)
        .bind(expires_at_millis)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(StoreError::from)
    }

    /// Returns whether an unexpired revocation exists for a token.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for malformed identifiers or database failure.
    pub async fn is_access_token_revoked(
        &self,
        subject: &str,
        nonce: u64,
    ) -> Result<bool, StoreError> {
        validate_hex_id(subject)?;
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM fluvora_token_revocations \
             WHERE subject_id = $1 AND token_nonce = $2::NUMERIC \
               AND expires_at > clock_timestamp())",
        )
        .bind(subject)
        .bind(nonce.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)
    }

    /// Deletes expired revocation tombstones in bounded batches.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for an invalid limit or database failure.
    pub async fn purge_expired_token_revocations(&self, limit: u32) -> Result<u64, StoreError> {
        if !(1..=10_000).contains(&limit) {
            return Err(StoreError::InvalidBatchSize(limit));
        }
        sqlx::query(
            "DELETE FROM fluvora_token_revocations WHERE ctid IN (\
               SELECT ctid FROM fluvora_token_revocations \
               WHERE expires_at <= clock_timestamp() ORDER BY expires_at LIMIT $1\
             )",
        )
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(StoreError::from)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RoomRow {
    room_id: String,
    creation_command_id: String,
    revision: i64,
    snapshot: Value,
    ended: bool,
}

impl TryFrom<RoomRow> for StoredRoom {
    type Error = StoreError;

    fn try_from(row: RoomRow) -> Result<Self, Self::Error> {
        Ok(Self {
            room_id: row.room_id,
            creation_command_id: row.creation_command_id,
            revision: to_u64(row.revision, "revision")?,
            snapshot: row.snapshot,
            ended: row.ended,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct LeaseRow {
    resource_kind: String,
    resource_id: String,
    owner_id: String,
    generation: i64,
    metadata: Value,
}

impl TryFrom<LeaseRow> for Lease {
    type Error = StoreError;

    fn try_from(row: LeaseRow) -> Result<Self, Self::Error> {
        Ok(Self {
            resource_kind: row.resource_kind,
            resource_id: row.resource_id,
            owner_id: row.owner_id,
            generation: to_u64(row.generation, "lease generation")?,
            metadata: row.metadata,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct SignalRow {
    room_id: String,
    sequence: i64,
    command_id: String,
    from_id: String,
    to_id: Option<String>,
    kind: String,
    payload: Value,
    timestamp_millis: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ServicePlacementRow {
    resource_kind: String,
    resource_id: String,
    node_id: String,
    generation: i64,
    endpoint: String,
}

impl TryFrom<ServicePlacementRow> for ServicePlacement {
    type Error = StoreError;

    fn try_from(row: ServicePlacementRow) -> Result<Self, Self::Error> {
        Ok(Self {
            resource_kind: row.resource_kind,
            resource_id: row.resource_id,
            node_id: row.node_id,
            endpoint: row.endpoint,
            generation: to_u64(row.generation, "service placement generation")?,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ServiceNodeRow {
    node_id: String,
    endpoint: String,
}

impl TryFrom<SignalRow> for StoredSignal {
    type Error = StoreError;

    fn try_from(row: SignalRow) -> Result<Self, Self::Error> {
        Ok(Self {
            room_id: row.room_id,
            sequence: to_u64(row.sequence, "signal sequence")?,
            command_id: row.command_id,
            from_id: row.from_id,
            to_id: row.to_id,
            kind: row.kind,
            payload: row.payload,
            timestamp_millis: to_u64(row.timestamp_millis, "signal timestamp")?,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct OutboxRow {
    id: i64,
    aggregate_type: String,
    aggregate_id: String,
    aggregate_sequence: i64,
    event_type: String,
    payload: Value,
    attempts: i32,
}

impl TryFrom<OutboxRow> for OutboxMessage {
    type Error = StoreError;

    fn try_from(row: OutboxRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            aggregate_type: row.aggregate_type,
            aggregate_id: row.aggregate_id,
            aggregate_sequence: to_u64(row.aggregate_sequence, "aggregate sequence")?,
            event_type: row.event_type,
            payload: row.payload,
            attempts: u32::try_from(row.attempts)
                .map_err(|_| StoreError::NumericRange("outbox attempts"))?,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct NodeRow {
    node_id: String,
    endpoint: String,
    ice_candidate: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct PlacementWithEndpointRow {
    room_id: String,
    node_id: String,
    endpoint: String,
    ice_candidate: Option<String>,
    generation: i64,
}

impl TryFrom<PlacementWithEndpointRow> for RoomPlacement {
    type Error = StoreError;

    fn try_from(row: PlacementWithEndpointRow) -> Result<Self, Self::Error> {
        Ok(Self {
            room_id: row.room_id,
            node_id: row.node_id,
            endpoint: row.endpoint,
            ice_candidate: row.ice_candidate,
            generation: to_u64(row.generation, "placement generation")?,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PlacementRow {
    room_id: String,
    node_id: String,
    generation: i64,
}

impl RoomPlacement {
    fn from_row(
        row: PlacementRow,
        endpoint: String,
        ice_candidate: Option<String>,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            room_id: row.room_id,
            node_id: row.node_id,
            endpoint,
            ice_candidate,
            generation: to_u64(row.generation, "placement generation")?,
        })
    }
}

async fn apply_migration(
    transaction: &mut Transaction<'_, Postgres>,
    version: i64,
    name: &str,
    sql: &str,
) -> Result<(), StoreError> {
    let checksum = checksum(sql);
    let existing = sqlx::query_as::<_, (String,)>(
        "SELECT checksum FROM fluvora_schema_migrations WHERE version = $1",
    )
    .bind(version)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    if let Some((actual,)) = existing {
        if actual != checksum {
            return Err(StoreError::MigrationChecksumMismatch { version });
        }
        return Ok(());
    }
    sqlx::raw_sql(sql)
        .execute(&mut **transaction)
        .await
        .map_err(StoreError::from)?;
    sqlx::query(
        "INSERT INTO fluvora_schema_migrations (version, name, checksum) VALUES ($1, $2, $3)",
    )
    .bind(version)
    .bind(name)
    .bind(checksum)
    .execute(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    Ok(())
}

async fn load_room_by_creation(
    transaction: &mut Transaction<'_, Postgres>,
    command_id: &str,
) -> Result<Option<StoredRoom>, StoreError> {
    sqlx::query_as::<_, RoomRow>(
        "SELECT room_id, creation_command_id, revision, snapshot, ended \
         FROM fluvora_rooms WHERE creation_command_id = $1",
    )
    .bind(command_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(StoreError::from)?
    .map(StoredRoom::try_from)
    .transpose()
}

async fn event_exists(
    transaction: &mut Transaction<'_, Postgres>,
    room_id: &str,
    command_id: &str,
) -> Result<bool, StoreError> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM fluvora_room_events WHERE room_id = $1 AND command_id = $2)",
    )
    .bind(room_id)
    .bind(command_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(StoreError::from)
}

async fn insert_event_and_outbox(
    transaction: &mut Transaction<'_, Postgres>,
    room_id: &str,
    event: &EventWrite,
    sequence: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO fluvora_room_events (room_id, sequence, command_id, event) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(room_id)
    .bind(sequence)
    .bind(&event.command_id)
    .bind(&event.event)
    .execute(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    sqlx::query(
        "INSERT INTO fluvora_outbox \
         (aggregate_type, aggregate_id, aggregate_sequence, event_type, payload) \
         VALUES ('room', $1, $2, $3, $4)",
    )
    .bind(room_id)
    .bind(sequence)
    .bind(&event.event_type)
    .bind(&event.event)
    .execute(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    Ok(())
}

async fn insert_gift(
    transaction: &mut Transaction<'_, Postgres>,
    room_id: &str,
    sequence: i64,
    gift: &GiftLedgerWrite,
) -> Result<(), StoreError> {
    let quantity = i32::try_from(gift.quantity).map_err(|_| StoreError::InvalidGift)?;
    let unit_value = to_i64(gift.unit_value, "gift unit value")?;
    sqlx::query(
        "INSERT INTO fluvora_gift_ledger \
         (transaction_id, room_id, event_sequence, sender_id, recipient_id, gift_id, quantity, \
          unit_value, total_value, currency) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::numeric, $10)",
    )
    .bind(&gift.transaction_id)
    .bind(room_id)
    .bind(sequence)
    .bind(&gift.sender_id)
    .bind(&gift.recipient_id)
    .bind(&gift.gift_id)
    .bind(quantity)
    .bind(unit_value)
    .bind(gift.total_value.to_string())
    .bind(&gift.currency)
    .execute(&mut **transaction)
    .await
    .map(|_| ())
    .map_err(|error| {
        if is_unique_violation(&error) {
            StoreError::DuplicateGiftTransaction
        } else {
            StoreError::from(error)
        }
    })
}

async fn load_healthy_placement(
    transaction: &mut Transaction<'_, Postgres>,
    room_id: &str,
    stale_millis: i64,
) -> Result<Option<RoomPlacement>, StoreError> {
    sqlx::query_as::<_, PlacementWithEndpointRow>(
        "SELECT placement.room_id, placement.node_id, node.endpoint, node.ice_candidate, \
                placement.generation \
         FROM fluvora_room_placements AS placement \
         JOIN fluvora_media_nodes AS node ON node.node_id = placement.node_id \
         WHERE placement.room_id = $1 AND node.healthy AND NOT node.draining \
           AND node.heartbeat_at > \
               clock_timestamp() - ($2 * interval '1 millisecond') \
         FOR UPDATE OF placement",
    )
    .bind(room_id)
    .bind(stale_millis)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(StoreError::from)?
    .map(RoomPlacement::try_from)
    .transpose()
}

async fn select_media_node(
    transaction: &mut Transaction<'_, Postgres>,
    region: &str,
    stale_millis: i64,
) -> Result<Option<NodeRow>, StoreError> {
    sqlx::query_as::<_, NodeRow>(
        "SELECT node.node_id, node.endpoint, node.ice_candidate \
         FROM fluvora_media_nodes AS node \
         LEFT JOIN (\
           SELECT node_id, count(*)::bigint AS placed_rooms \
           FROM fluvora_room_placements GROUP BY node_id\
         ) AS placement ON placement.node_id = node.node_id \
         WHERE node.region = $1 AND node.healthy AND NOT node.draining \
           AND node.heartbeat_at > \
               clock_timestamp() - ($2 * interval '1 millisecond') \
           AND greatest(node.rooms_used, coalesce(placement.placed_rooms, 0)) < node.rooms_limit \
           AND node.sessions_used < node.sessions_limit \
         ORDER BY \
           (greatest(node.rooms_used, coalesce(placement.placed_rooms, 0))::numeric \
             / node.rooms_limit::numeric) ASC, \
           (node.sessions_used::numeric / node.sessions_limit::numeric) ASC, \
           node.publisher_tracks ASC, node.node_id ASC \
         FOR UPDATE OF node SKIP LOCKED LIMIT 1",
    )
    .bind(region)
    .bind(stale_millis)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(StoreError::from)
}

fn validate_room_write(room: &StoredRoom, event: &EventWrite) -> Result<(), StoreError> {
    validate_hex_id(&room.room_id)?;
    validate_hex_id(&room.creation_command_id)?;
    validate_hex_id(&event.command_id)?;
    validate_bounded_text(&event.event_type, "event type", 128)?;
    if event.sequence == 0 || room.revision == 0 {
        return Err(StoreError::InvalidInitialRevision);
    }
    let snapshot_bytes = serde_json::to_vec(&room.snapshot)
        .map_err(|error| StoreError::InvalidJson(error.to_string()))?
        .len();
    if snapshot_bytes > MAX_ROOM_SNAPSHOT_BYTES {
        return Err(StoreError::RoomSnapshotTooLarge(snapshot_bytes));
    }
    let event_bytes = serde_json::to_vec(&event.event)
        .map_err(|error| StoreError::InvalidJson(error.to_string()))?
        .len();
    if event_bytes > MAX_ROOM_EVENT_BYTES {
        return Err(StoreError::RoomEventTooLarge(event_bytes));
    }
    Ok(())
}

fn validate_media_node(heartbeat: &MediaNodeHeartbeat) -> Result<(), StoreError> {
    validate_bounded_text(&heartbeat.node_id, "node id", 256)?;
    validate_bounded_text(&heartbeat.region, "region", 128)?;
    validate_bounded_text(&heartbeat.endpoint, "node endpoint", 2_048)?;
    if heartbeat.ice_candidate.as_ref().is_some_and(|candidate| {
        candidate.is_empty()
            || candidate.len() > 2_048
            || candidate.bytes().any(|byte| byte.is_ascii_control())
    }) {
        return Err(StoreError::InvalidMediaNodeCapacity);
    }
    if !valid_node_field(&heartbeat.node_id, 256)
        || !valid_node_field(&heartbeat.region, 128)
        || !valid_http_origin(&heartbeat.endpoint)
        || heartbeat.rooms_limit == 0
        || heartbeat.sessions_limit == 0
        || heartbeat.rooms_used > heartbeat.rooms_limit
        || heartbeat.sessions_used > heartbeat.sessions_limit
    {
        return Err(StoreError::InvalidMediaNodeCapacity);
    }
    if serde_json::to_vec(&heartbeat.metadata)
        .map_err(|error| StoreError::InvalidJson(error.to_string()))?
        .len()
        > 65_536
    {
        return Err(StoreError::MetadataTooLarge);
    }
    Ok(())
}

fn validate_service_node(heartbeat: &ServiceNodeHeartbeat) -> Result<(), StoreError> {
    validate_bounded_text(&heartbeat.node_id, "node id", 256)?;
    validate_bounded_text(&heartbeat.service_kind, "service kind", 64)?;
    validate_bounded_text(&heartbeat.region, "region", 128)?;
    validate_bounded_text(&heartbeat.endpoint, "node endpoint", 2_048)?;
    if !valid_node_field(&heartbeat.node_id, 256)
        || !valid_node_field(&heartbeat.service_kind, 64)
        || !valid_node_field(&heartbeat.region, 128)
        || !valid_http_origin(&heartbeat.endpoint)
        || heartbeat.jobs_limit == 0
        || heartbeat.jobs_used > heartbeat.jobs_limit
    {
        return Err(StoreError::InvalidServiceNodeCapacity);
    }
    if serde_json::to_vec(&heartbeat.metadata)
        .map_err(|error| StoreError::InvalidJson(error.to_string()))?
        .len()
        > 65_536
    {
        return Err(StoreError::MetadataTooLarge);
    }
    Ok(())
}

fn valid_node_field(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_http_origin(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && matches!(parsed.path(), "" | "/")
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

fn checked_stale_millis(stale_after: Duration) -> Result<i64, StoreError> {
    let stale_millis = stale_after.as_millis();
    if !(1_000..=300_000).contains(&stale_millis) {
        return Err(StoreError::InvalidStaleWindow(stale_millis));
    }
    i64::try_from(stale_millis).map_err(|_| StoreError::InvalidStaleWindow(stale_millis))
}

fn validate_gift(gift: &GiftLedgerWrite) -> Result<(), StoreError> {
    validate_bounded_text(&gift.transaction_id, "transaction id", 512)?;
    validate_hex_id(&gift.sender_id)?;
    validate_hex_id(&gift.recipient_id)?;
    validate_bounded_text(&gift.gift_id, "gift id", 256)?;
    if gift.quantity == 0
        || gift.currency.len() != 3
        || !gift.currency.bytes().all(|byte| byte.is_ascii_uppercase())
        || gift.total_value != u128::from(gift.unit_value) * u128::from(gift.quantity)
    {
        return Err(StoreError::InvalidGift);
    }
    Ok(())
}

fn validate_signal(signal: &StoredSignal) -> Result<(), StoreError> {
    validate_hex_id(&signal.room_id)?;
    validate_hex_id(&signal.command_id)?;
    validate_hex_id(&signal.from_id)?;
    if let Some(to_id) = &signal.to_id {
        validate_hex_id(to_id)?;
    }
    validate_bounded_text(&signal.kind, "signal kind", 64)?;
    let payload_size = serde_json::to_vec(&signal.payload)
        .map_err(|error| StoreError::InvalidJson(error.to_string()))?
        .len();
    if payload_size > 65_536 {
        return Err(StoreError::SignalPayloadTooLarge(payload_size));
    }
    Ok(())
}

fn validate_hex_id(value: &str) -> Result<(), StoreError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(StoreError::InvalidIdentifier)
    }
}

fn validate_bounded_text(
    value: &str,
    field: &'static str,
    maximum: usize,
) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(StoreError::InvalidTextField { field, maximum })
    } else {
        Ok(())
    }
}

fn checksum(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::NumericRange(field))
}

fn to_u64(value: i64, field: &'static str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::NumericRange(field))
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database| database.code().as_deref() == Some("23505"))
}

/// Transactional store failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The configured pool size is outside its safe bound.
    #[error("invalid PostgreSQL pool size {0}")]
    InvalidPoolSize(u32),
    /// A 128-bit identifier was not lowercase hexadecimal.
    #[error("identifier must contain exactly 32 lowercase hexadecimal characters")]
    InvalidIdentifier,
    /// A bounded text field was empty, oversized, or contained a control character.
    #[error("{field} must contain 1..={maximum} non-control UTF-8 bytes")]
    InvalidTextField {
        /// Human-readable field name.
        field: &'static str,
        /// Maximum accepted UTF-8 bytes.
        maximum: usize,
    },
    /// A migration version was edited after it had been applied.
    #[error("database migration {version} checksum does not match")]
    MigrationChecksumMismatch {
        /// Conflicting migration version.
        version: i64,
    },
    /// Room or command identifier collided unexpectedly.
    #[error("room identifier collided with an unrelated creation command")]
    IdentifierCollision,
    /// An initial event or snapshot did not start at revision one.
    #[error("initial room revision and event sequence must both equal one")]
    InvalidInitialRevision,
    /// A room update skipped or repeated a revision.
    #[error("invalid room revision transition: expected {expected}, got {actual}")]
    InvalidRevisionTransition {
        /// Required next revision.
        expected: u64,
        /// Supplied revision.
        actual: u64,
    },
    /// The requested durable room does not exist.
    #[error("durable room does not exist")]
    RoomNotFound,
    /// `PostgreSQL` did not update a row held with `FOR UPDATE`.
    #[error("PostgreSQL row lock was lost")]
    LostDatabaseLock,
    /// A numeric field does not fit `PostgreSQL`'s signed representation.
    #[error("{0} exceeds the PostgreSQL numeric range")]
    NumericRange(&'static str),
    /// A gift entry failed its invariant checks.
    #[error("invalid gift ledger entry")]
    InvalidGift,
    /// A provider transaction already exists in the immutable ledger.
    #[error("duplicate gift provider transaction")]
    DuplicateGiftTransaction,
    /// Lease duration was outside 1 second through 5 minutes.
    #[error("invalid lease TTL {0} milliseconds")]
    InvalidLeaseTtl(u128),
    /// Outbox claim exceeded the bounded batch size.
    #[error("invalid outbox batch size {0}")]
    InvalidBatchSize(u32),
    /// Outbox identifier was not positive.
    #[error("invalid outbox message id {0}")]
    InvalidOutboxId(i64),
    /// Outbox retry delay exceeded one hour.
    #[error("invalid outbox retry delay {0} milliseconds")]
    InvalidRetryDelay(u128),
    /// Delivered outbox retention was outside one hour through one year.
    #[error("invalid delivered outbox retention {0} milliseconds")]
    InvalidOutboxRetention(u128),
    /// A compact aggregate snapshot exceeded its durable storage bound.
    #[error("room snapshot contains {0} bytes; maximum is 33554432")]
    RoomSnapshotTooLarge(usize),
    /// A single room event exceeded its durable storage bound.
    #[error("room event contains {0} bytes; maximum is 1048576")]
    RoomEventTooLarge(usize),
    /// A signaling payload exceeds its bounded storage and transport limit.
    #[error("signal payload contains {0} bytes; maximum is 65536")]
    SignalPayloadTooLarge(usize),
    /// Media-node reported an invalid endpoint or capacity.
    #[error("invalid media-node endpoint or capacity")]
    InvalidMediaNodeCapacity,
    /// Generic service node reported an invalid endpoint or capacity.
    #[error("invalid service-node endpoint or capacity")]
    InvalidServiceNodeCapacity,
    /// Media-node metadata exceeded 64 KiB.
    #[error("media-node metadata exceeds 64 KiB")]
    MetadataTooLarge,
    /// JSON serialization failed for bounded metadata.
    #[error("invalid JSON metadata: {0}")]
    InvalidJson(String),
    /// Node heartbeat expiry window was outside 1 second through 5 minutes.
    #[error("invalid media-node stale window {0} milliseconds")]
    InvalidStaleWindow(u128),
    /// No healthy media node had capacity in the requested region.
    #[error("no healthy media node has placement capacity")]
    NoMediaNodeCapacity,
    /// No healthy generic service node had capacity in the requested region.
    #[error("no healthy service node has placement capacity")]
    NoServiceNodeCapacity,
    /// `PostgreSQL` connection, query, or transaction failure.
    #[error("PostgreSQL error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        EventWrite, GiftLedgerWrite, MIGRATIONS, MediaNodeHeartbeat, ServiceNodeHeartbeat,
        StoreError, StoredRoom, checksum, validate_gift, validate_hex_id, validate_media_node,
        validate_room_write, validate_service_node,
    };

    fn room() -> StoredRoom {
        StoredRoom {
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            creation_command_id: "11111111111111111111111111111111".to_owned(),
            revision: 1,
            snapshot: json!({"schema_version": 1}),
            ended: false,
        }
    }

    fn event() -> EventWrite {
        EventWrite {
            sequence: 1,
            command_id: "22222222222222222222222222222222".to_owned(),
            event_type: "room.created".to_owned(),
            event: json!({"type": "created"}),
        }
    }

    #[test]
    fn validates_ids_and_room_writes() {
        assert!(validate_room_write(&room(), &event()).is_ok());
        assert_eq!(validate_hex_id("ABC"), Err(StoreError::InvalidIdentifier));
        let mut invalid = room();
        invalid.room_id = "../room".to_owned();
        assert_eq!(
            validate_room_write(&invalid, &event()),
            Err(StoreError::InvalidIdentifier)
        );
    }

    #[test]
    fn validates_exact_gift_totals_and_currency() {
        let gift = GiftLedgerWrite {
            transaction_id: "provider-1".to_owned(),
            sender_id: "33333333333333333333333333333333".to_owned(),
            recipient_id: "44444444444444444444444444444444".to_owned(),
            gift_id: "rocket".to_owned(),
            quantity: 3,
            unit_value: 500,
            total_value: 1_500,
            currency: "CNY".to_owned(),
        };
        assert!(validate_gift(&gift).is_ok());
        let mut invalid = gift;
        invalid.total_value += 1;
        assert_eq!(validate_gift(&invalid), Err(StoreError::InvalidGift));
    }

    #[test]
    fn embeds_ordered_nonempty_migrations_with_stable_checksums() {
        assert!(!MIGRATIONS.is_empty());
        let versions = MIGRATIONS
            .iter()
            .map(|(version, _, _)| *version)
            .collect::<Vec<_>>();
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
        for (_, _, sql) in MIGRATIONS {
            assert!(!sql.trim().is_empty());
            assert_eq!(checksum(sql).len(), 64);
        }
    }

    #[test]
    fn rejects_forged_node_origins_and_identifiers() {
        let media = MediaNodeHeartbeat {
            node_id: "media-a".to_owned(),
            region: "cn-east".to_owned(),
            endpoint: "http://media-a:8092".to_owned(),
            ice_candidate: None,
            healthy: true,
            draining: false,
            rooms_used: 0,
            rooms_limit: 10,
            sessions_used: 0,
            sessions_limit: 100,
            publisher_tracks: 0,
            metadata: json!({}),
        };
        assert!(validate_media_node(&media).is_ok());
        for endpoint in [
            "http://token@media-a:8092",
            "http://media-a:8092/internal",
            "http://media-a:8092?redirect=true",
            "file:///tmp/media.sock",
        ] {
            let mut forged = media.clone();
            forged.endpoint = endpoint.to_owned();
            assert_eq!(
                validate_media_node(&forged),
                Err(StoreError::InvalidMediaNodeCapacity)
            );
        }
        let mut forged = media;
        forged.node_id = "media/a".to_owned();
        assert_eq!(
            validate_media_node(&forged),
            Err(StoreError::InvalidMediaNodeCapacity)
        );

        let service = ServiceNodeHeartbeat {
            node_id: "worker-a".to_owned(),
            service_kind: "media_worker".to_owned(),
            region: "cn-east".to_owned(),
            endpoint: "https://worker-a:8093".to_owned(),
            healthy: true,
            draining: false,
            jobs_used: 0,
            jobs_limit: 10,
            metadata: json!({}),
        };
        assert!(validate_service_node(&service).is_ok());
        let mut forged = service;
        forged.endpoint = "https://worker-a:8093/v1/jobs".to_owned();
        assert_eq!(
            validate_service_node(&forged),
            Err(StoreError::InvalidServiceNodeCapacity)
        );
    }
}
