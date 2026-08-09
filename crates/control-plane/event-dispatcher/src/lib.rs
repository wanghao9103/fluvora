//! Durable transactional-outbox event envelopes and routing.

use std::time::Duration;

use fluvora_control_store::OutboxMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current wire schema of [`EventEnvelope`].
pub const EVENT_SCHEMA_VERSION: u16 = 1;

/// Stable event published to the Fluvora event bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Wire schema version.
    pub schema_version: u16,
    /// Globally stable event identifier used for consumer idempotency.
    pub event_id: String,
    /// Monotonic outbox row identifier.
    pub outbox_id: i64,
    /// Aggregate category.
    pub aggregate_type: String,
    /// Aggregate identifier.
    pub aggregate_id: String,
    /// Aggregate-local event sequence.
    pub aggregate_sequence: u64,
    /// Stable domain event type.
    pub event_type: String,
    /// Versioned domain payload.
    pub payload: Value,
}

impl From<&OutboxMessage> for EventEnvelope {
    fn from(message: &OutboxMessage) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: format!("outbox-{}", message.id),
            outbox_id: message.id,
            aggregate_type: message.aggregate_type.clone(),
            aggregate_id: message.aggregate_id.clone(),
            aggregate_sequence: message.aggregate_sequence,
            event_type: message.event_type.clone(),
            payload: message.payload.clone(),
        }
    }
}

/// Creates a bounded NATS subject below the configured root.
#[must_use]
pub fn event_subject(root: &str, message: &OutboxMessage) -> String {
    format!(
        "{}.{}.{}",
        root.trim_end_matches('.'),
        subject_token(&message.aggregate_type),
        subject_tokens(&message.event_type)
    )
}

/// Exponential retry delay capped at one minute.
#[must_use]
pub fn retry_delay(attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(6);
    Duration::from_secs(1_u64 << exponent)
}

fn subject_tokens(value: &str) -> String {
    value
        .split('.')
        .map(subject_token)
        .collect::<Vec<_>>()
        .join(".")
}

fn subject_token(value: &str) -> String {
    let token = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                char::from(byte.to_ascii_lowercase())
            } else {
                '_'
            }
        })
        .take(128)
        .collect::<String>();
    if token.is_empty() {
        "unknown".to_owned()
    } else {
        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(attempts: u32) -> OutboxMessage {
        OutboxMessage {
            id: 42,
            aggregate_type: "Room".to_owned(),
            aggregate_id: "0123456789abcdef0123456789abcdef".to_owned(),
            aggregate_sequence: 7,
            event_type: "gift.sent".to_owned(),
            payload: serde_json::json!({"quantity": 2}),
            attempts,
        }
    }

    #[test]
    fn builds_stable_versioned_envelope_and_subject() {
        let message = message(1);
        let envelope = EventEnvelope::from(&message);
        assert_eq!(envelope.schema_version, EVENT_SCHEMA_VERSION);
        assert_eq!(envelope.event_id, "outbox-42");
        assert_eq!(
            event_subject("fluvora.events.", &message),
            "fluvora.events.room.gift.sent"
        );
    }

    #[test]
    fn sanitizes_subject_tokens_and_bounds_backoff() {
        let mut message = message(100);
        message.aggregate_type = "Room * unsafe".to_owned();
        message.event_type = "chat.>".to_owned();
        assert_eq!(
            event_subject("fluvora.events", &message),
            "fluvora.events.room___unsafe.chat._"
        );
        assert_eq!(retry_delay(1), Duration::from_secs(1));
        assert_eq!(retry_delay(100), Duration::from_secs(64));
    }
}
