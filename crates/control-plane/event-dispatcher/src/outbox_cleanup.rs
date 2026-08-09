use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fluvora_control_store::PostgresStore;

use super::Metrics;

pub(super) async fn run(
    store: PostgresStore,
    retention: Duration,
    batch_size: u32,
    metrics: Arc<Metrics>,
) {
    let mut interval = tokio::time::interval(Duration::from_mins(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        for _ in 0..10 {
            match store.prune_delivered_outbox(retention, batch_size).await {
                Ok(pruned) => {
                    metrics.pruned.fetch_add(pruned, Ordering::Relaxed);
                    if pruned < u64::from(batch_size) {
                        break;
                    }
                }
                Err(error) => {
                    eprintln!("delivered outbox cleanup failed: {error}");
                    break;
                }
            }
        }
    }
}
