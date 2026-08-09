use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream;
use fluvora_control_store::{CreateRoomOutcome, EventWrite, PostgresStore, StoredRoom};
use fluvora_event_dispatcher::{EventEnvelope, event_subject};
use futures_util::StreamExt as _;
use serde_json::json;

fn unique_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!("{:032x}", time ^ u128::from(std::process::id()))
}

async fn create_room(store: &PostgresStore) -> StoredRoom {
    let room_id = unique_id();
    let command_id = unique_id();
    let room = StoredRoom {
        room_id: room_id.clone(),
        creation_command_id: command_id.clone(),
        revision: 1,
        snapshot: json!({"schema_version": 1, "revision": 1}),
        ended: false,
    };
    let event = EventWrite {
        sequence: 1,
        command_id,
        event_type: "room.created".to_owned(),
        event: json!({"schema_version": 1, "room_id": room_id}),
    };
    assert_eq!(
        store
            .create_room(&room, &event)
            .await
            .expect("create test room"),
        CreateRoomOutcome::Created
    );
    room
}

#[tokio::test]
async fn publishes_outbox_with_jetstream_deduplication() {
    let (Ok(database_url), Ok(nats_url), Ok(nats_token)) = (
        std::env::var("FLUVORA_TEST_DATABASE_URL"),
        std::env::var("FLUVORA_TEST_NATS_URL"),
        std::env::var("FLUVORA_TEST_NATS_TOKEN"),
    ) else {
        eprintln!(
            "skipping JetStream integration test: FLUVORA_TEST_DATABASE_URL, \
             FLUVORA_TEST_NATS_URL, and FLUVORA_TEST_NATS_TOKEN are required"
        );
        return;
    };
    let store = PostgresStore::connect(&database_url, 4)
        .await
        .expect("connect test PostgreSQL");
    store.migrate().await.expect("apply migrations");
    let room = create_room(&store).await;

    let client = async_nats::ConnectOptions::with_token(nats_token)
        .connect(nats_url)
        .await
        .expect("connect test NATS");
    let context = jetstream::new(client.clone());
    let stream_name = format!("TEST_{}", unique_id().to_ascii_uppercase());
    let subject_root = format!("fluvora.test.{}", unique_id());
    context
        .create_stream(jetstream::stream::Config {
            name: stream_name.clone(),
            subjects: vec![format!("{subject_root}.>")],
            duplicate_window: Duration::from_mins(2),
            ..Default::default()
        })
        .await
        .expect("create test stream");
    let mut subscriber = client
        .subscribe(format!("{subject_root}.>"))
        .await
        .expect("subscribe to test subject");

    let owner = unique_id();
    let messages = store
        .claim_outbox(&owner, 100, Duration::from_secs(5))
        .await
        .expect("claim outbox");
    let message = messages
        .iter()
        .find(|message| message.aggregate_id == room.room_id)
        .expect("created room outbox message");
    let envelope = EventEnvelope::from(message);
    let payload = serde_json::to_vec(&envelope).expect("serialize envelope");
    let subject = event_subject(&subject_root, message);
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", envelope.event_id.as_str());

    let first_ack = context
        .publish_with_headers(subject.clone(), headers.clone(), payload.clone().into())
        .await
        .expect("publish first event")
        .await
        .expect("first JetStream acknowledgement");
    assert!(!first_ack.duplicate);
    let duplicate_ack = context
        .publish_with_headers(subject, headers, payload.into())
        .await
        .expect("publish duplicate event")
        .await
        .expect("duplicate JetStream acknowledgement");
    assert!(duplicate_ack.duplicate);

    let delivered = tokio::time::timeout(Duration::from_secs(3), subscriber.next())
        .await
        .expect("event delivery timeout")
        .expect("event subscription ended");
    let delivered: EventEnvelope =
        serde_json::from_slice(&delivered.payload).expect("decode delivered envelope");
    assert_eq!(delivered, envelope);
    assert!(
        store
            .acknowledge_outbox(&owner, message.id)
            .await
            .expect("acknowledge outbox")
    );
    context
        .delete_stream(stream_name)
        .await
        .expect("delete test stream");
}
