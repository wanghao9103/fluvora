//! Event-sourced room, membership, chat, gift, and extension-data domain model.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Default maximum UTF-8 bytes in a durable chat message.
pub const MAX_CHAT_BYTES: usize = 4_096;
/// Maximum UTF-8 bytes in a custom-data namespace.
pub const MAX_CUSTOM_NAMESPACE_BYTES: usize = 64;
/// Default maximum encoded application bytes in durable custom data.
pub const MAX_CUSTOM_DATA_BYTES: usize = 60 * 1_024;

/// The human-readable platform name.
pub const PLATFORM_NAME: &str = "Fluvora";

const COMMAND_HISTORY_LIMIT: usize = 4_096;
const GIFT_TRANSACTION_HISTORY_LIMIT: usize = 4_096;
const MAX_GIFT_TRANSACTION_ID_BYTES: usize = 512;
const MAX_GIFT_ID_BYTES: usize = 256;

/// Globally unique room identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomId(pub u128);

/// Globally unique authenticated user identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub u128);

/// Client-generated idempotency identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(pub u128);

macro_rules! impl_hex_id_serde {
    ($id:ty, $name:literal) => {
        impl Serialize for $id {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&format!("{:032x}", self.0))
            }
        }

        impl<'de> Deserialize<'de> for $id {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct IdVisitor;

                impl<'de> serde::de::Visitor<'de> for IdVisitor {
                    type Value = u128;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str(concat!("a 32-character hexadecimal ", $name))
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                        {
                            return Err(E::invalid_value(serde::de::Unexpected::Str(value), &self));
                        }
                        u128::from_str_radix(value, 16).map_err(E::custom)
                    }

                    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        Ok(u128::from(value))
                    }

                    fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        Ok(value)
                    }
                }

                deserializer.deserialize_any(IdVisitor).map(Self)
            }
        }
    };
}

impl_hex_id_serde!(RoomId, "room identifier");
impl_hex_id_serde!(UserId, "user identifier");
impl_hex_id_serde!(CommandId, "command identifier");

/// Room media topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomMode {
    /// Server-routed selective forwarding.
    Sfu,
    /// Server-signaled direct browser peer connection.
    P2p,
    /// Host/publisher stage with a large audience.
    Live,
    /// Stored playback session; realtime viewers cannot publish.
    Vod,
}

/// Participant permission level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    /// Room owner.
    Host,
    /// Delegated moderator and publisher.
    CoHost,
    /// Participant allowed to publish media.
    Publisher,
    /// Receive-only participant.
    Audience,
}

impl MemberRole {
    const fn can_publish(self) -> bool {
        matches!(self, Self::Host | Self::CoHost | Self::Publisher)
    }

    const fn can_moderate(self) -> bool {
        matches!(self, Self::Host | Self::CoHost)
    }
}

/// Bounded room policy persisted with the aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPolicy {
    /// Maximum simultaneous members.
    pub max_members: usize,
    /// Maximum simultaneous publishers.
    pub max_publishers: usize,
    /// Maximum UTF-8 chat bytes.
    pub max_chat_bytes: usize,
    /// Maximum application extension bytes.
    pub max_custom_data_bytes: usize,
}

impl Default for RoomPolicy {
    fn default() -> Self {
        Self {
            max_members: 10_000,
            max_publishers: 32,
            max_chat_bytes: MAX_CHAT_BYTES,
            max_custom_data_bytes: MAX_CUSTOM_DATA_BYTES,
        }
    }
}

/// Server-verified payment result used to emit a gift event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedGiftReceipt {
    /// Payment-provider transaction identifier.
    pub transaction_id: String,
    /// Catalog gift identifier.
    pub gift_id: String,
    /// Number of gifts.
    pub quantity: u32,
    /// Value of one gift in the smallest currency unit.
    pub unit_value: u64,
    /// Uppercase ISO-style currency code.
    pub currency: String,
    /// Gift recipient.
    pub recipient: UserId,
}

/// Application-defined versioned data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomData {
    /// Reverse-domain or product namespace.
    pub namespace: String,
    /// Application schema version.
    pub schema_version: u16,
    /// Opaque payload.
    pub payload: Vec<u8>,
}

/// A validated room command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RoomCommand {
    /// Join as receive-only audience.
    Join { user: UserId },
    /// Leave the room.
    Leave { user: UserId },
    /// Change a member's role.
    SetRole {
        /// Moderator issuing the change.
        actor: UserId,
        /// Member being changed.
        user: UserId,
        /// New role.
        role: MemberRole,
    },
    /// Begin publishing media.
    StartPublishing { user: UserId },
    /// Stop publishing media.
    StopPublishing { user: UserId },
    /// Send a chat message.
    SendChat {
        /// Authenticated sender.
        user: UserId,
        /// Client message identifier.
        message_id: u128,
        /// UTF-8 message text.
        text: String,
    },
    /// Record a gift after external payment verification.
    RecordVerifiedGift {
        /// Paying user.
        sender: UserId,
        /// Verified transaction.
        receipt: VerifiedGiftReceipt,
    },
    /// Broadcast application-defined data.
    SendCustomData {
        /// Authenticated sender.
        user: UserId,
        /// Extension payload.
        data: CustomData,
    },
    /// Permanently end the room.
    End { actor: UserId },
}

/// Persisted room state change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RoomEventKind {
    /// Initial room creation.
    Created {
        /// Host user.
        host: UserId,
        /// Media topology.
        mode: RoomMode,
    },
    /// Member joined.
    MemberJoined { user: UserId, role: MemberRole },
    /// Member left.
    MemberLeft { user: UserId },
    /// Member role changed.
    RoleChanged {
        /// Changed user.
        user: UserId,
        /// New role.
        role: MemberRole,
    },
    /// Member began publishing.
    PublishingStarted { user: UserId },
    /// Member stopped publishing.
    PublishingStopped { user: UserId },
    /// Chat accepted.
    ChatSent {
        /// Sender.
        user: UserId,
        /// Client message identifier.
        message_id: u128,
        /// Text.
        text: String,
    },
    /// Verified gift accepted.
    GiftRecorded {
        /// Sender.
        sender: UserId,
        /// Payment receipt.
        receipt: VerifiedGiftReceipt,
        /// Checked total value in the smallest currency unit.
        total_value: u64,
    },
    /// Custom application data accepted.
    CustomDataSent { user: UserId, data: CustomData },
    /// Room permanently ended.
    Ended { actor: UserId },
}

/// Ordered event-store record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomEvent {
    /// Room event-stream sequence, starting at one.
    pub sequence: u64,
    /// Idempotency key that produced the event.
    pub command_id: CommandId,
    /// Server Unix timestamp in milliseconds.
    pub timestamp_millis: u64,
    /// State change.
    pub kind: RoomEventKind,
}

/// Result of idempotent command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// A new event was applied and should be persisted/published.
    Applied(RoomEvent),
    /// The bounded aggregate history already contains this command.
    Duplicate,
}

/// Event-sourced room aggregate.
#[derive(Debug, Clone)]
pub struct Room {
    id: RoomId,
    mode: RoomMode,
    policy: RoomPolicy,
    members: HashMap<UserId, MemberRole>,
    publishers: HashSet<UserId>,
    ended: bool,
    sequence: u64,
    processed_commands: HashSet<CommandId>,
    command_order: VecDeque<CommandId>,
    gift_transactions: HashSet<String>,
    gift_transaction_order: VecDeque<String>,
}

/// Versioned, bounded current-state representation used by durable room snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSnapshot {
    schema_version: u8,
    id: RoomId,
    mode: RoomMode,
    policy: RoomPolicy,
    members: Vec<(UserId, MemberRole)>,
    publishers: Vec<UserId>,
    ended: bool,
    sequence: u64,
    command_order: Vec<CommandId>,
    gift_transaction_order: Vec<String>,
}

impl Room {
    /// Creates a room and its first persistable event.
    #[must_use]
    pub fn create(
        id: RoomId,
        mode: RoomMode,
        host: UserId,
        policy: RoomPolicy,
        command_id: CommandId,
        timestamp_millis: u64,
    ) -> (Self, RoomEvent) {
        let event = RoomEvent {
            sequence: 1,
            command_id,
            timestamp_millis,
            kind: RoomEventKind::Created { host, mode },
        };
        let mut room = Self::empty(id, mode, policy);
        room.apply(&event);
        (room, event)
    }

    /// Restores an aggregate from an exact ordered event stream.
    ///
    /// # Errors
    ///
    /// Returns [`RoomError`] for an empty stream, a non-creation first event, wrong mode, or a
    /// sequence gap.
    pub fn restore(
        id: RoomId,
        mode: RoomMode,
        policy: RoomPolicy,
        events: &[RoomEvent],
    ) -> Result<Self, RoomError> {
        let first = events.first().ok_or(RoomError::EmptyEventStream)?;
        if !matches!(
            first.kind,
            RoomEventKind::Created {
                mode: event_mode,
                ..
            } if event_mode == mode
        ) {
            return Err(RoomError::InvalidCreationEvent);
        }
        let mut room = Self::empty(id, mode, policy);
        for event in events {
            let expected = room.sequence + 1;
            if event.sequence != expected {
                return Err(RoomError::EventSequenceGap {
                    expected,
                    actual: event.sequence,
                });
            }
            room.apply(event);
        }
        Ok(room)
    }

    /// Captures current aggregate state without retaining historical chat/custom payloads.
    #[must_use]
    pub fn snapshot(&self) -> RoomSnapshot {
        let mut members = self
            .members
            .iter()
            .map(|(user, role)| (*user, *role))
            .collect::<Vec<_>>();
        members.sort_by_key(|(user, _)| *user);
        let mut publishers = self.publishers.iter().copied().collect::<Vec<_>>();
        publishers.sort_unstable();
        RoomSnapshot {
            schema_version: 1,
            id: self.id,
            mode: self.mode,
            policy: self.policy.clone(),
            members,
            publishers,
            ended: self.ended,
            sequence: self.sequence,
            command_order: self.command_order.iter().copied().collect(),
            gift_transaction_order: self.gift_transaction_order.iter().cloned().collect(),
        }
    }

    /// Restores and validates a bounded current-state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`RoomError::InvalidSnapshot`] when identities, capacities, roles, histories, or
    /// sequence state violate aggregate invariants.
    pub fn restore_snapshot(snapshot: RoomSnapshot) -> Result<Self, RoomError> {
        let members = snapshot.members.iter().copied().collect::<HashMap<_, _>>();
        let publishers = snapshot.publishers.iter().copied().collect::<HashSet<_>>();
        let processed_commands = snapshot
            .command_order
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let gift_transactions = snapshot
            .gift_transaction_order
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let policy = &snapshot.policy;
        let policy_valid = (2..=100_000).contains(&policy.max_members)
            && (1..=1_024).contains(&policy.max_publishers)
            && (1..=MAX_CHAT_BYTES).contains(&policy.max_chat_bytes)
            && (1..=MAX_CUSTOM_DATA_BYTES).contains(&policy.max_custom_data_bytes);
        let histories_valid = !snapshot.command_order.is_empty()
            && snapshot.command_order.len() <= COMMAND_HISTORY_LIMIT
            && processed_commands.len() == snapshot.command_order.len()
            && snapshot.gift_transaction_order.len() <= GIFT_TRANSACTION_HISTORY_LIMIT
            && gift_transactions.len() == snapshot.gift_transaction_order.len()
            && snapshot
                .gift_transaction_order
                .iter()
                .all(|transaction| valid_gift_text(transaction, MAX_GIFT_TRANSACTION_ID_BYTES));
        let membership_valid = !members.is_empty()
            && members.len() == snapshot.members.len()
            && members.len() <= policy.max_members
            && members
                .values()
                .filter(|role| **role == MemberRole::Host)
                .count()
                == 1;
        let publishers_valid = publishers.len() == snapshot.publishers.len()
            && publishers.len() <= policy.max_publishers
            && publishers
                .iter()
                .all(|user| members.get(user).is_some_and(|role| role.can_publish()))
            && (!snapshot.ended || publishers.is_empty());
        if snapshot.schema_version != 1
            || snapshot.sequence == 0
            || !policy_valid
            || !histories_valid
            || !membership_valid
            || !publishers_valid
        {
            return Err(RoomError::InvalidSnapshot);
        }
        Ok(Self {
            id: snapshot.id,
            mode: snapshot.mode,
            policy: snapshot.policy,
            members,
            publishers,
            ended: snapshot.ended,
            sequence: snapshot.sequence,
            processed_commands,
            command_order: VecDeque::from(snapshot.command_order),
            gift_transactions,
            gift_transaction_order: VecDeque::from(snapshot.gift_transaction_order),
        })
    }

    /// Executes, applies, and returns one idempotent command.
    ///
    /// # Errors
    ///
    /// Returns [`RoomError`] when membership, permission, state, or resource invariants fail.
    pub fn execute(
        &mut self,
        command_id: CommandId,
        timestamp_millis: u64,
        command: RoomCommand,
    ) -> Result<CommandOutcome, RoomError> {
        if self.processed_commands.contains(&command_id) {
            return Ok(CommandOutcome::Duplicate);
        }
        if self.ended {
            return Err(RoomError::RoomEnded);
        }
        let kind = self.decide(command)?;
        let event = RoomEvent {
            sequence: self
                .sequence
                .checked_add(1)
                .ok_or(RoomError::SequenceExhausted)?,
            command_id,
            timestamp_millis,
            kind,
        };
        self.apply(&event);
        Ok(CommandOutcome::Applied(event))
    }

    /// Returns the room identifier.
    #[must_use]
    pub const fn id(&self) -> RoomId {
        self.id
    }

    /// Returns the room media topology.
    #[must_use]
    pub const fn mode(&self) -> RoomMode {
        self.mode
    }

    /// Returns the bounded room policy persisted with this aggregate.
    #[must_use]
    pub const fn policy(&self) -> &RoomPolicy {
        &self.policy
    }

    /// Returns the last applied event sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns whether the room has permanently ended.
    #[must_use]
    pub const fn is_ended(&self) -> bool {
        self.ended
    }

    /// Returns a member's role.
    #[must_use]
    pub fn member_role(&self, user: UserId) -> Option<MemberRole> {
        self.members.get(&user).copied()
    }

    /// Returns whether a member is actively publishing.
    #[must_use]
    pub fn is_publishing(&self, user: UserId) -> bool {
        self.publishers.contains(&user)
    }

    /// Returns the current member count without exposing identities.
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Returns the current active publisher count.
    #[must_use]
    pub fn publisher_count(&self) -> usize {
        self.publishers.len()
    }

    fn empty(id: RoomId, mode: RoomMode, policy: RoomPolicy) -> Self {
        Self {
            id,
            mode,
            policy,
            members: HashMap::new(),
            publishers: HashSet::new(),
            ended: false,
            sequence: 0,
            processed_commands: HashSet::new(),
            command_order: VecDeque::new(),
            gift_transactions: HashSet::new(),
            gift_transaction_order: VecDeque::new(),
        }
    }

    fn decide(&self, command: RoomCommand) -> Result<RoomEventKind, RoomError> {
        match command {
            RoomCommand::Join { user } => self.decide_join(user),
            RoomCommand::Leave { user } => self.decide_leave(user),
            RoomCommand::SetRole { actor, user, role } => {
                self.decide_role_change(actor, user, role)
            }
            RoomCommand::StartPublishing { user } => self.decide_start_publishing(user),
            RoomCommand::StopPublishing { user } => self.decide_stop_publishing(user),
            RoomCommand::SendChat {
                user,
                message_id,
                text,
            } => self.decide_chat(user, message_id, text),
            RoomCommand::RecordVerifiedGift { sender, receipt } => {
                self.decide_gift(sender, receipt)
            }
            RoomCommand::SendCustomData { user, data } => self.decide_custom_data(user, data),
            RoomCommand::End { actor } => self.decide_end(actor),
        }
    }

    fn decide_join(&self, user: UserId) -> Result<RoomEventKind, RoomError> {
        if self.members.contains_key(&user) {
            return Err(RoomError::AlreadyMember(user));
        }
        if self.members.len() >= self.policy.max_members {
            return Err(RoomError::MemberLimit);
        }
        Ok(RoomEventKind::MemberJoined {
            user,
            role: MemberRole::Audience,
        })
    }

    fn decide_leave(&self, user: UserId) -> Result<RoomEventKind, RoomError> {
        match self.require_member(user)? {
            MemberRole::Host => Err(RoomError::HostCannotLeave),
            _ => Ok(RoomEventKind::MemberLeft { user }),
        }
    }

    fn decide_role_change(
        &self,
        actor: UserId,
        user: UserId,
        role: MemberRole,
    ) -> Result<RoomEventKind, RoomError> {
        let actor_role = self.require_member(actor)?;
        let current = self.require_member(user)?;
        if !actor_role.can_moderate()
            || (actor_role == MemberRole::CoHost
                && matches!(current, MemberRole::Host | MemberRole::CoHost))
            || current == MemberRole::Host
            || role == MemberRole::Host
        {
            return Err(RoomError::PermissionDenied(actor));
        }
        Ok(RoomEventKind::RoleChanged { user, role })
    }

    fn decide_start_publishing(&self, user: UserId) -> Result<RoomEventKind, RoomError> {
        let role = self.require_member(user)?;
        if self.mode == RoomMode::Vod || !role.can_publish() {
            return Err(RoomError::PermissionDenied(user));
        }
        if self.publishers.contains(&user) {
            return Err(RoomError::AlreadyPublishing(user));
        }
        if self.publishers.len() >= self.policy.max_publishers {
            return Err(RoomError::PublisherLimit);
        }
        Ok(RoomEventKind::PublishingStarted { user })
    }

    fn decide_stop_publishing(&self, user: UserId) -> Result<RoomEventKind, RoomError> {
        self.require_member(user)?;
        if !self.publishers.contains(&user) {
            return Err(RoomError::NotPublishing(user));
        }
        Ok(RoomEventKind::PublishingStopped { user })
    }

    fn decide_chat(
        &self,
        user: UserId,
        message_id: u128,
        text: String,
    ) -> Result<RoomEventKind, RoomError> {
        self.require_member(user)?;
        if text.is_empty() || text.len() > self.policy.max_chat_bytes {
            return Err(RoomError::InvalidChatLength(text.len()));
        }
        Ok(RoomEventKind::ChatSent {
            user,
            message_id,
            text,
        })
    }

    fn decide_gift(
        &self,
        sender: UserId,
        receipt: VerifiedGiftReceipt,
    ) -> Result<RoomEventKind, RoomError> {
        self.require_member(sender)?;
        self.require_member(receipt.recipient)?;
        if !valid_gift_text(&receipt.transaction_id, MAX_GIFT_TRANSACTION_ID_BYTES)
            || !valid_gift_text(&receipt.gift_id, MAX_GIFT_ID_BYTES)
            || receipt.quantity == 0
            || receipt.currency.len() != 3
            || !receipt
                .currency
                .bytes()
                .all(|byte| byte.is_ascii_uppercase())
        {
            return Err(RoomError::InvalidGiftReceipt);
        }
        if self.gift_transactions.contains(&receipt.transaction_id) {
            return Err(RoomError::DuplicateGiftTransaction);
        }
        let total_value = receipt
            .unit_value
            .checked_mul(u64::from(receipt.quantity))
            .ok_or(RoomError::GiftValueOverflow)?;
        Ok(RoomEventKind::GiftRecorded {
            sender,
            receipt,
            total_value,
        })
    }

    fn decide_custom_data(
        &self,
        user: UserId,
        data: CustomData,
    ) -> Result<RoomEventKind, RoomError> {
        self.require_member(user)?;
        if !valid_custom_namespace(&data.namespace)
            || data.payload.len() > self.policy.max_custom_data_bytes
        {
            return Err(RoomError::InvalidCustomData);
        }
        Ok(RoomEventKind::CustomDataSent { user, data })
    }

    fn decide_end(&self, actor: UserId) -> Result<RoomEventKind, RoomError> {
        if self.require_member(actor)? != MemberRole::Host {
            return Err(RoomError::PermissionDenied(actor));
        }
        Ok(RoomEventKind::Ended { actor })
    }

    fn require_member(&self, user: UserId) -> Result<MemberRole, RoomError> {
        self.members
            .get(&user)
            .copied()
            .ok_or(RoomError::NotMember(user))
    }

    fn apply(&mut self, event: &RoomEvent) {
        match &event.kind {
            RoomEventKind::Created { host, .. } => {
                self.members.insert(*host, MemberRole::Host);
            }
            RoomEventKind::MemberJoined { user, role }
            | RoomEventKind::RoleChanged { user, role } => {
                self.members.insert(*user, *role);
                if !role.can_publish() {
                    self.publishers.remove(user);
                }
            }
            RoomEventKind::MemberLeft { user } => {
                self.members.remove(user);
                self.publishers.remove(user);
            }
            RoomEventKind::PublishingStarted { user } => {
                self.publishers.insert(*user);
            }
            RoomEventKind::PublishingStopped { user } => {
                self.publishers.remove(user);
            }
            RoomEventKind::GiftRecorded { receipt, .. } => {
                self.remember_gift_transaction(&receipt.transaction_id);
            }
            RoomEventKind::Ended { .. } => {
                self.publishers.clear();
                self.ended = true;
            }
            RoomEventKind::ChatSent { .. } | RoomEventKind::CustomDataSent { .. } => {}
        }
        self.sequence = event.sequence;
        self.remember_command(event.command_id);
    }

    fn remember_command(&mut self, command_id: CommandId) {
        if self.processed_commands.insert(command_id) {
            self.command_order.push_back(command_id);
        }
        while self.command_order.len() > COMMAND_HISTORY_LIMIT {
            if let Some(expired) = self.command_order.pop_front() {
                self.processed_commands.remove(&expired);
            }
        }
    }

    fn remember_gift_transaction(&mut self, transaction_id: &str) {
        if self.gift_transactions.insert(transaction_id.to_owned()) {
            self.gift_transaction_order
                .push_back(transaction_id.to_owned());
        }
        while self.gift_transaction_order.len() > GIFT_TRANSACTION_HISTORY_LIMIT {
            if let Some(expired) = self.gift_transaction_order.pop_front() {
                self.gift_transactions.remove(&expired);
            }
        }
    }
}

fn valid_custom_namespace(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_CUSTOM_NAMESPACE_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_gift_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

/// Room command or restoration failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomError {
    /// Event stream was empty.
    EmptyEventStream,
    /// First event was not a matching creation event.
    InvalidCreationEvent,
    /// A current-state snapshot violated aggregate invariants.
    InvalidSnapshot,
    /// Event sequence was not contiguous.
    EventSequenceGap {
        /// Required next sequence.
        expected: u64,
        /// Supplied sequence.
        actual: u64,
    },
    /// Event sequence reached `u64::MAX`.
    SequenceExhausted,
    /// Room is permanently ended.
    RoomEnded,
    /// User is already a member.
    AlreadyMember(UserId),
    /// User is not a member.
    NotMember(UserId),
    /// Actor lacks the required role.
    PermissionDenied(UserId),
    /// Host must end the room instead of leaving.
    HostCannotLeave,
    /// Member bound reached.
    MemberLimit,
    /// Publisher bound reached.
    PublisherLimit,
    /// User already publishes.
    AlreadyPublishing(UserId),
    /// User is not publishing.
    NotPublishing(UserId),
    /// Chat is empty or exceeds policy.
    InvalidChatLength(usize),
    /// Gift fields are malformed.
    InvalidGiftReceipt,
    /// Payment transaction was already applied.
    DuplicateGiftTransaction,
    /// Gift quantity multiplication overflowed.
    GiftValueOverflow,
    /// Custom namespace or payload violates policy.
    InvalidCustomData,
}

impl fmt::Display for RoomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEventStream => formatter.write_str("room event stream is empty"),
            Self::InvalidCreationEvent => formatter.write_str("invalid room creation event"),
            Self::InvalidSnapshot => formatter.write_str("invalid room state snapshot"),
            Self::EventSequenceGap { expected, actual } => {
                write!(
                    formatter,
                    "room event sequence gap: expected {expected}, got {actual}"
                )
            }
            Self::SequenceExhausted => formatter.write_str("room event sequence exhausted"),
            Self::RoomEnded => formatter.write_str("room has ended"),
            Self::AlreadyMember(user) => write!(formatter, "user {user:?} already joined"),
            Self::NotMember(user) => write!(formatter, "user {user:?} is not in room"),
            Self::PermissionDenied(user) => write!(formatter, "permission denied for {user:?}"),
            Self::HostCannotLeave => formatter.write_str("host must end the room"),
            Self::MemberLimit => formatter.write_str("room member limit reached"),
            Self::PublisherLimit => formatter.write_str("room publisher limit reached"),
            Self::AlreadyPublishing(user) => write!(formatter, "user {user:?} already publishes"),
            Self::NotPublishing(user) => write!(formatter, "user {user:?} is not publishing"),
            Self::InvalidChatLength(length) => write!(formatter, "invalid chat length {length}"),
            Self::InvalidGiftReceipt => formatter.write_str("invalid verified gift receipt"),
            Self::DuplicateGiftTransaction => formatter.write_str("duplicate gift transaction"),
            Self::GiftValueOverflow => formatter.write_str("gift value overflow"),
            Self::InvalidCustomData => formatter.write_str("invalid custom room data"),
        }
    }
}

impl std::error::Error for RoomError {}

#[cfg(test)]
mod tests {
    use super::{
        CommandId, CommandOutcome, CustomData, MemberRole, Room, RoomCommand, RoomError, RoomId,
        RoomMode, RoomPolicy, UserId, VerifiedGiftReceipt,
    };

    fn created_room() -> (Room, Vec<super::RoomEvent>) {
        let (room, event) = Room::create(
            RoomId(1),
            RoomMode::Live,
            UserId(10),
            RoomPolicy::default(),
            CommandId(1),
            100,
        );
        (room, vec![event])
    }

    #[test]
    fn serializes_ids_as_lossless_hex_and_reads_legacy_numbers() {
        let room_id = RoomId(u128::MAX - 1);
        let (_, event) = Room::create(
            room_id,
            RoomMode::Live,
            UserId(u128::MAX - 2),
            RoomPolicy::default(),
            CommandId(u128::MAX - 3),
            100,
        );
        let encoded = serde_json::to_string(&event).expect("serialize event");
        assert!(encoded.contains("fffffffffffffffffffffffffffffffc"));
        let decoded: super::RoomEvent = serde_json::from_str(&encoded).expect("restore event");
        assert_eq!(decoded, event);

        assert_eq!(
            serde_json::from_str::<RoomId>("42").expect("legacy numeric ID"),
            RoomId(42)
        );
        assert_eq!(
            serde_json::from_str::<RoomId>("\"0000000000000000000000000000002a\"").expect("hex ID"),
            RoomId(42)
        );
        assert!(serde_json::from_str::<RoomId>("\"2a\"").is_err());
    }

    fn apply(
        room: &mut Room,
        events: &mut Vec<super::RoomEvent>,
        command_id: u128,
        command: RoomCommand,
    ) {
        let outcome = room
            .execute(
                CommandId(command_id),
                100 + u64::try_from(command_id).expect("small test command id"),
                command,
            )
            .expect("valid command");
        let CommandOutcome::Applied(event) = outcome else {
            panic!("expected applied event");
        };
        events.push(event);
    }

    #[test]
    fn enforces_roles_and_restores_exact_state() {
        let (mut room, mut events) = created_room();
        apply(
            &mut room,
            &mut events,
            2,
            RoomCommand::Join { user: UserId(20) },
        );
        assert_eq!(
            room.execute(
                CommandId(3),
                103,
                RoomCommand::StartPublishing { user: UserId(20) }
            ),
            Err(RoomError::PermissionDenied(UserId(20)))
        );
        apply(
            &mut room,
            &mut events,
            4,
            RoomCommand::SetRole {
                actor: UserId(10),
                user: UserId(20),
                role: MemberRole::Publisher,
            },
        );
        apply(
            &mut room,
            &mut events,
            5,
            RoomCommand::StartPublishing { user: UserId(20) },
        );
        assert!(room.is_publishing(UserId(20)));

        let restored = Room::restore(RoomId(1), RoomMode::Live, RoomPolicy::default(), &events)
            .expect("restore event stream");
        assert_eq!(restored.sequence(), room.sequence());
        assert_eq!(
            restored.member_role(UserId(20)),
            Some(MemberRole::Publisher)
        );
        assert!(restored.is_publishing(UserId(20)));
        let restored_snapshot = Room::restore_snapshot(room.snapshot()).expect("restore snapshot");
        assert_eq!(restored_snapshot.sequence(), room.sequence());
        assert_eq!(
            restored_snapshot.member_role(UserId(20)),
            Some(MemberRole::Publisher)
        );
        assert!(restored_snapshot.is_publishing(UserId(20)));
        assert_eq!(
            room.execute(
                CommandId(6),
                106,
                RoomCommand::SetRole {
                    actor: UserId(10),
                    user: UserId(10),
                    role: MemberRole::Audience,
                },
            ),
            Err(RoomError::PermissionDenied(UserId(10)))
        );
    }

    #[test]
    fn makes_commands_idempotent_and_gifts_transaction_unique() {
        let (mut room, mut events) = created_room();
        apply(
            &mut room,
            &mut events,
            2,
            RoomCommand::Join { user: UserId(20) },
        );
        let receipt = VerifiedGiftReceipt {
            transaction_id: "pay-1".to_owned(),
            gift_id: "rocket".to_owned(),
            quantity: 2,
            unit_value: 500,
            currency: "CNY".to_owned(),
            recipient: UserId(10),
        };
        let command = RoomCommand::RecordVerifiedGift {
            sender: UserId(20),
            receipt: receipt.clone(),
        };
        let applied = room
            .execute(CommandId(3), 103, command.clone())
            .expect("valid gift");
        assert!(matches!(applied, CommandOutcome::Applied(_)));
        assert_eq!(
            room.execute(CommandId(3), 104, command.clone()),
            Ok(CommandOutcome::Duplicate)
        );
        assert_eq!(
            room.execute(
                CommandId(4),
                105,
                RoomCommand::RecordVerifiedGift {
                    sender: UserId(20),
                    receipt
                }
            ),
            Err(RoomError::DuplicateGiftTransaction)
        );
    }

    #[test]
    fn rejects_unbounded_or_malformed_gift_fields() {
        let (mut room, mut events) = created_room();
        apply(
            &mut room,
            &mut events,
            2,
            RoomCommand::Join { user: UserId(20) },
        );
        let valid = VerifiedGiftReceipt {
            transaction_id: "pay-1".to_owned(),
            gift_id: "rocket".to_owned(),
            quantity: 2,
            unit_value: 500,
            currency: "CNY".to_owned(),
            recipient: UserId(10),
        };
        let malformed = [
            VerifiedGiftReceipt {
                transaction_id: "x".repeat(513),
                ..valid.clone()
            },
            VerifiedGiftReceipt {
                transaction_id: "pay\n1".to_owned(),
                ..valid.clone()
            },
            VerifiedGiftReceipt {
                gift_id: "x".repeat(257),
                ..valid.clone()
            },
            VerifiedGiftReceipt {
                currency: "cny".to_owned(),
                ..valid.clone()
            },
            VerifiedGiftReceipt {
                quantity: 0,
                ..valid
            },
        ];

        for (index, receipt) in malformed.into_iter().enumerate() {
            assert_eq!(
                room.execute(
                    CommandId(100 + index as u128),
                    200 + index as u64,
                    RoomCommand::RecordVerifiedGift {
                        sender: UserId(20),
                        receipt,
                    },
                ),
                Err(RoomError::InvalidGiftReceipt)
            );
        }
    }

    #[test]
    fn validates_chat_and_custom_data_bounds() {
        let (mut room, mut events) = created_room();
        apply(
            &mut room,
            &mut events,
            2,
            RoomCommand::Join { user: UserId(20) },
        );
        apply(
            &mut room,
            &mut events,
            3,
            RoomCommand::SendChat {
                user: UserId(20),
                message_id: 7,
                text: "hello".to_owned(),
            },
        );
        apply(
            &mut room,
            &mut events,
            4,
            RoomCommand::SendCustomData {
                user: UserId(20),
                data: CustomData {
                    namespace: "com.example.poll".to_owned(),
                    schema_version: 1,
                    payload: vec![1, 2, 3],
                },
            },
        );
        assert_eq!(room.sequence(), 4);

        assert_eq!(
            room.execute(
                CommandId(5),
                5,
                RoomCommand::SendChat {
                    user: UserId(20),
                    message_id: 8,
                    text: "x".repeat(super::MAX_CHAT_BYTES + 1),
                },
            ),
            Err(RoomError::InvalidChatLength(super::MAX_CHAT_BYTES + 1))
        );
        for (command_id, namespace) in [(6, ".invalid"), (7, "invalid\nnamespace")] {
            assert_eq!(
                room.execute(
                    CommandId(command_id),
                    u64::try_from(command_id).expect("small command id"),
                    RoomCommand::SendCustomData {
                        user: UserId(20),
                        data: CustomData {
                            namespace: namespace.to_owned(),
                            schema_version: 1,
                            payload: vec![1],
                        },
                    },
                ),
                Err(RoomError::InvalidCustomData)
            );
        }
        assert_eq!(
            room.execute(
                CommandId(8),
                8,
                RoomCommand::SendCustomData {
                    user: UserId(20),
                    data: CustomData {
                        namespace: "com.example.large".to_owned(),
                        schema_version: 1,
                        payload: vec![0; super::MAX_CUSTOM_DATA_BYTES + 1],
                    },
                },
            ),
            Err(RoomError::InvalidCustomData)
        );
    }
}
