use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluvora_control_store::{
    AppendOutcome, CreateRoomOutcome, EventWrite, GiftLedgerWrite, MediaNodeHeartbeat,
    PostgresStore, ServiceNodeHeartbeat, StoredRoom, StoredSignal,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn unique_id(label: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let digest = Sha256::digest(format!("{label}-{}-{now}", std::process::id()).as_bytes());
    digest[..16]
        .iter()
        .fold(String::with_capacity(32), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn event(sequence: u64, command_id: String, event_type: &str) -> EventWrite {
    EventWrite {
        sequence,
        command_id,
        event_type: event_type.to_owned(),
        event: json!({"schema_version": 1, "sequence": sequence}),
    }
}

#[tokio::test]
async fn commits_idempotency_outbox_gift_and_fenced_leases() {
    let Ok(database_url) = std::env::var("FLUVORA_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: FLUVORA_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PostgresStore::connect(&database_url, 4)
        .await
        .expect("connect test PostgreSQL");
    store.migrate().await.expect("apply migrations");
    let room_id = verify_room_transactions(&store).await;
    verify_signals(&store, &room_id).await;
    verify_outbox(&store, &room_id).await;
    verify_placement(&store, &room_id).await;
    verify_service_placement(&store).await;
    verify_fenced_leases(&store).await;
    verify_token_revocations(&store).await;
}

async fn verify_token_revocations(store: &PostgresStore) {
    let subject = unique_id("revoked-subject");
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .saturating_add(60_000);
    let expires = u64::try_from(expires).expect("timestamp");
    store
        .revoke_access_token(&subject, u64::MAX, expires, "integration test")
        .await
        .expect("revoke token");
    assert!(
        store
            .is_access_token_revoked(&subject, u64::MAX)
            .await
            .expect("query revocation")
    );
    assert!(
        !store
            .is_access_token_revoked(&subject, u64::MAX - 1)
            .await
            .expect("query unrelated nonce")
    );
}

async fn verify_service_placement(store: &PostgresStore) {
    let node_a = ServiceNodeHeartbeat {
        node_id: unique_id("worker-a"),
        service_kind: "media_worker".to_owned(),
        region: "test".to_owned(),
        endpoint: "http://worker-a:8091".to_owned(),
        healthy: true,
        draining: false,
        jobs_used: 0,
        jobs_limit: 4,
        metadata: json!({}),
    };
    let node_b = ServiceNodeHeartbeat {
        node_id: unique_id("worker-b"),
        endpoint: "http://worker-b:8091".to_owned(),
        jobs_used: 1,
        ..node_a.clone()
    };
    store
        .upsert_service_node(&node_a)
        .await
        .expect("register worker A");
    store
        .upsert_service_node(&node_b)
        .await
        .expect("register worker B");
    let resource_id = unique_id("realtime-job");
    let first = store
        .place_service_resource(
            "realtime_job",
            &resource_id,
            "media_worker",
            "test",
            Duration::from_secs(15),
        )
        .await
        .expect("place realtime job");
    assert_eq!(first.node_id, node_a.node_id);
    let duplicate = store
        .place_service_resource(
            "realtime_job",
            &resource_id,
            "media_worker",
            "test",
            Duration::from_secs(15),
        )
        .await
        .expect("reuse realtime job placement");
    assert_eq!(duplicate, first);
    let restarted = store
        .advance_service_placement(
            "realtime_job",
            &resource_id,
            "media_worker",
            "test",
            Duration::from_secs(15),
        )
        .await
        .expect("advance realtime job fence");
    assert_eq!(restarted.node_id, first.node_id);
    assert_eq!(restarted.generation, first.generation + 1);
    store
        .upsert_service_node(&ServiceNodeHeartbeat {
            draining: true,
            ..node_a
        })
        .await
        .expect("drain worker A");
    let failed_over = store
        .place_service_resource(
            "realtime_job",
            &resource_id,
            "media_worker",
            "test",
            Duration::from_secs(15),
        )
        .await
        .expect("fail over realtime job");
    assert_eq!(failed_over.node_id, node_b.node_id);
    assert_eq!(failed_over.generation, restarted.generation + 1);
    verify_generation_fenced_removal(
        store,
        &resource_id,
        restarted.generation,
        failed_over.generation,
    )
    .await;
}

async fn verify_generation_fenced_removal(
    store: &PostgresStore,
    resource_id: &str,
    stale_generation: u64,
    current_generation: u64,
) {
    assert!(
        !store
            .remove_service_placement_generation("realtime_job", resource_id, stale_generation,)
            .await
            .expect("preserve newer realtime job placement")
    );
    assert!(
        store
            .remove_service_placement_generation("realtime_job", resource_id, current_generation,)
            .await
            .expect("remove realtime job placement")
    );
}

async fn verify_signals(store: &PostgresStore, room_id: &str) {
    let recipient = unique_id("signal-recipient");
    let signal = StoredSignal {
        room_id: room_id.to_owned(),
        sequence: 0,
        command_id: unique_id("signal-command"),
        from_id: unique_id("signal-sender"),
        to_id: Some(recipient.clone()),
        kind: "offer".to_owned(),
        payload: json!({"type": "offer", "sdp": "v=0\r\n"}),
        timestamp_millis: 1_700_000_000_000,
    };
    let (first, duplicate) = tokio::join!(
        store.append_room_signal(&signal),
        store.append_room_signal(&signal)
    );
    let first = first.expect("append signal");
    let duplicate = duplicate.expect("idempotent concurrent signal");
    assert_eq!(first, duplicate);
    assert_eq!(first.sequence, 1);
    let page = store
        .load_room_signal_page(room_id, 0, 100, &recipient)
        .await
        .expect("load recipient signals");
    assert_eq!(page.signals, vec![first]);
    assert_eq!(page.latest_sequence, 1);
    let filtered = store
        .load_room_signal_page(room_id, 0, 100, &unique_id("other-recipient"))
        .await
        .expect("load other recipient signals");
    assert!(filtered.signals.is_empty());
    assert_eq!(filtered.latest_sequence, 1);
}

async fn verify_room_transactions(store: &PostgresStore) -> String {
    let room_id = unique_id("room");
    let creation_command_id = unique_id("create");
    let first_event = event(1, creation_command_id.clone(), "room.created");
    let initial = StoredRoom {
        room_id: room_id.clone(),
        creation_command_id: creation_command_id.clone(),
        revision: 1,
        snapshot: json!({"schema_version": 1, "revision": 1}),
        ended: false,
    };
    assert_eq!(
        store
            .create_room(&initial, &first_event)
            .await
            .expect("create room"),
        CreateRoomOutcome::Created
    );

    let duplicate = StoredRoom {
        room_id: unique_id("ignored-room"),
        ..initial.clone()
    };
    assert!(matches!(
        store
            .create_room(&duplicate, &first_event)
            .await
            .expect("idempotent create"),
        CreateRoomOutcome::Duplicate(stored) if stored.room_id == room_id
    ));

    let gift_command = unique_id("gift-command");
    let second_event = event(2, gift_command.clone(), "room.gift_recorded");
    let updated = StoredRoom {
        revision: 2,
        snapshot: json!({"schema_version": 1, "revision": 2}),
        ..initial.clone()
    };
    let gift = GiftLedgerWrite {
        transaction_id: unique_id("provider-transaction"),
        sender_id: unique_id("sender"),
        recipient_id: unique_id("recipient"),
        gift_id: "rocket".to_owned(),
        quantity: 2,
        unit_value: 500,
        total_value: 1_000,
        currency: "CNY".to_owned(),
    };
    assert_eq!(
        store
            .append_room_event(&updated, 1, &second_event, Some(&gift))
            .await
            .expect("append gift"),
        AppendOutcome::Applied
    );
    assert!(matches!(
        store
            .append_room_event(&updated, 1, &second_event, Some(&gift))
            .await
            .expect("duplicate append"),
        AppendOutcome::Duplicate(stored) if stored.revision == 2
    ));

    let conflicting = StoredRoom {
        revision: 2,
        snapshot: json!({"schema_version": 1, "revision": 2}),
        ..initial
    };
    assert_eq!(
        store
            .append_room_event(
                &conflicting,
                1,
                &event(2, unique_id("conflict"), "room.member_joined"),
                None,
            )
            .await
            .expect("revision conflict"),
        AppendOutcome::RevisionConflict { actual_revision: 2 }
    );
    room_id
}

async fn verify_outbox(store: &PostgresStore, room_id: &str) {
    let owner = unique_id("outbox-owner");
    let messages = store
        .claim_outbox(&owner, 100, Duration::from_secs(5))
        .await
        .expect("claim outbox");
    let room_messages = messages
        .iter()
        .filter(|message| message.aggregate_id == room_id)
        .count();
    assert_eq!(room_messages, 3);
    for message in messages {
        assert!(
            store
                .acknowledge_outbox(&owner, message.id)
                .await
                .expect("acknowledge outbox")
        );
    }
}

async fn verify_placement(store: &PostgresStore, room_id: &str) {
    let node_a = MediaNodeHeartbeat {
        node_id: unique_id("node-a"),
        region: "test".to_owned(),
        endpoint: "http://node-a:8092".to_owned(),
        ice_candidate: Some("1 1 UDP 2130706431 203.0.113.10 50000 typ host".to_owned()),
        healthy: true,
        draining: false,
        rooms_used: 8,
        rooms_limit: 10,
        sessions_used: 80,
        sessions_limit: 100,
        publisher_tracks: 20,
        metadata: json!({}),
    };
    let node_b = MediaNodeHeartbeat {
        node_id: unique_id("node-b"),
        endpoint: "http://node-b:8092".to_owned(),
        rooms_used: 1,
        sessions_used: 10,
        publisher_tracks: 2,
        ..node_a.clone()
    };
    store
        .upsert_media_node(&node_a)
        .await
        .expect("register node A");
    store
        .upsert_media_node(&node_b)
        .await
        .expect("register node B");
    let first = store
        .place_room(room_id, "test", Duration::from_secs(15))
        .await
        .expect("place room");
    assert_eq!(first.node_id, node_b.node_id);
    assert_eq!(first.generation, 1);
    store
        .upsert_media_node(&MediaNodeHeartbeat {
            draining: true,
            ..node_b
        })
        .await
        .expect("drain node B");
    let replacement = store
        .place_room(room_id, "test", Duration::from_secs(15))
        .await
        .expect("replace placement");
    assert_eq!(replacement.node_id, node_a.node_id);
    assert_eq!(replacement.generation, 2);
}

async fn verify_fenced_leases(store: &PostgresStore) {
    let resource_id = unique_id("lease");
    let owner_a = unique_id("owner-a");
    let owner_b = unique_id("owner-b");
    let lease_a = store
        .acquire_lease(
            "worker_job",
            &resource_id,
            &owner_a,
            Duration::from_secs(1),
            &json!({"job": 7}),
        )
        .await
        .expect("acquire lease")
        .expect("available lease");
    assert_eq!(lease_a.generation, 1);
    assert!(
        store
            .acquire_lease(
                "worker_job",
                &resource_id,
                &owner_b,
                Duration::from_secs(1),
                &json!({}),
            )
            .await
            .expect("contended lease")
            .is_none()
    );
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let lease_b = store
        .acquire_lease(
            "worker_job",
            &resource_id,
            &owner_b,
            Duration::from_secs(1),
            &json!({}),
        )
        .await
        .expect("lease takeover")
        .expect("expired lease");
    assert_eq!(lease_b.generation, 2);
    assert!(!store.release_lease(&lease_a).await.expect("fenced release"));
    assert!(store.release_lease(&lease_b).await.expect("owner release"));
}
