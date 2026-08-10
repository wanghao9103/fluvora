# Fluvora Production v1 operations runbook

[简体中文](../RUNBOOK.md) | [English](RUNBOOK.md)

This runbook defines production release, rollback, backup/restore, and common incident procedures.
Before any command, verify the namespace, cluster, database, and exact target in a read-only context.
Never execute placeholders directly.

## 1. Pre-release checks

1. The tag's Release workflow is green; images have Cosign signatures, SLSA provenance, and SPDX
   SBOMs.
2. `FLUVORA_PUBLIC_IP`, TURN relay range, firewall, API/gateway ingress, DTLS/TURN certificates,
   and object-storage endpoint belong to the target environment.
3. Active/retiring token keys are ordered correctly, internal tokens are isolated, and all example
   secrets have been replaced.
4. PostgreSQL backup and object-storage versioning checks passed inside the change window.
5. Service-down, heartbeat, authentication, DataChannel exhaustion/abandonment, TURN pressure, and
   worker-failure alerts are deliverable.

## 2. Rollout and rollback

Use immutable image digests, never mutable tags. Expand traffic through 1% → 10% → 50% → 100%,
observing at least two alert windows per stage. Compare connection success, first-packet time, loss,
p95/p99, transcode failure, and 5xx.

Rollback when WebRTC/WHIP/WHEP success drops by more than 1%; 5xx/auth/DataChannel failures remain
abnormal for two windows; outbox, worker queue, or TURN ports continuously grow; or a release writes
irreversible data without a compatible down/forward-fix path.

Restore the previous signed digest. API/dispatcher can roll back first. Mark media nodes draining,
stop new placement, and wait for rooms before replacing them. Keep database migrations backward
compatible; destructive column cleanup waits at least one release.

## 3. PostgreSQL backup and restore

Business owners define RPO. Use continuous WAL archive and at least one daily logical or physical
full backup. Example logical backup:

```bash
pg_dump --format=custom --no-owner --no-acl \
  --dbname="$FLUVORA_DATABASE_URL" --file="fluvora-$(date -u +%Y%m%dT%H%M%SZ).dump"
pg_restore --list fluvora-YYYYMMDDTHHMMSSZ.dump >/dev/null
```

Restore only into an isolated database during drills:

```bash
createdb fluvora_restore_drill
pg_restore --exit-on-error --clean --if-exists --no-owner --no-acl \
  --dbname=fluvora_restore_drill fluvora-YYYYMMDDTHHMMSSZ.dump
```

Run migrations and read-only consistency checks before switching API traffic. Verify room revisions,
gift ledger, outbox, token revocation, service leases/placement generations, and signal sequences.
An old fencing generation must never regain write permission.

The event dispatcher prunes successfully delivered outbox rows older than the retention window in
batches. Defaults are 168 hours via `FLUVORA_OUTBOX_RETENTION_HOURS` (1-8760) and batch size via
`FLUVORA_OUTBOX_CLEANUP_BATCH` (1-10000). Pending/leased rows are never selected. Check JetStream
`max_age`, audit requirements, backup policy, and `fluvora_event_dispatcher_pruned_total` before
changing retention.

Single-process development without PostgreSQL uses `FLUVORA_STATE_DIR` and two-version atomic
snapshots. Startup validates filename, identity, revision, command history, and domain bounds and can
fall back one version. Asset/live metadata and worker assignment fences use the same strategy. This
mode is not for multi-replica production; never rename a corrupt file to a higher revision.

## 4. Object-storage recovery

- Enable versioning, server-side encryption, lifecycle policy, and deletion protection.
- Restore database asset/live metadata together with publication markers, not manifests alone.
- Verify every init segment, manifest reference, and checksum; leave assets with missing segments
  failed rather than publishing partial playlists.
- `FLUVORA_VOD_RETENTION_HOURS` and `FLUVORA_LIVE_RETENTION_HOURS` control normal retention.
  Legal hold uses a separate prefix/bucket policy.

## 5. Incident response

### Media node or worker exits

Confirm heartbeat expiry, advanced placement generation, and fencing of the old instance. During
real-time transcode reconstruction, observe `media.transcode_restarted`. After three probe failures
or a rejected failover, stop automatic retry, preserve task/placement evidence, and expand the
healthy pool. Cleanup must carry its generation; an old generation failing to delete current
placement is expected.

### Media gateway proxy failure

The API maps gateway connection failure, redirects, 5xx, responses over 1 MiB, and non-JSON control
bodies to 502. Check gateway readiness, `FLUVORA_GATEWAY_URL`, internal token, ingress/service
redirects, and HTML error pages. Do not loosen response validation or enable redirects.

### PostgreSQL or NATS unavailable

API readiness fails. Do not bypass it. After PostgreSQL recovery, inspect outbox backlog. After NATS
recovery, the dispatcher replays durable outbox rows and deduplicates by event ID. Never manually
skip unconfirmed events.

### Disk full or object-storage failure

Stop new upload/live/transcode admission while preserving reads. Delete only through lifecycle or
confirmed-deleted assets; never remove the shared media root directly. After capacity recovery,
verify temporary files, multipart uploads, and atomic publication markers.

### TURN port pressure

At 80%, expand/shard TURN nodes and a firewall/config-consistent relay range. Never reuse a port with
an active allocation. Inspect source IPs, nonce/auth failures, and TCP/TLS fallback ratio. Run
`fluvora-turn-probe` from the affected network for UDP, TCP, then TLS. The echo endpoint must be
outside the TURN node to prove a public relay path. Inject credentials by file or secret environment.

### Certificate expiry

Deploy the new certificate and synchronize the API SDP fingerprint with media-node identity before
draining old nodes. Renew TURN/TLS through the production CA. Stop new placement immediately on a
fingerprint mismatch.

## 6. Drill frequency and evidence

- Daily: CI/nightly, 30-minute PostgreSQL soak, protocol fuzz smoke, capacity benchmark.
- Weekly: worker/media-node crash, NATS reconnect, token rotation/revocation.
- Monthly: PostgreSQL restore, object version restore, certificate rotation, alert delivery.
- Quarterly: regional DR, staged rollback, 48-hour mixed-business soak.

Evidence includes version/digest, configuration summary, times, load, fault injection, metrics,
RTO/RPO, recovery actions, residual risk, and owner. A drill without traceable evidence does not
count toward Production v1 acceptance. Use `scripts/run-release-gates.ps1 -Profile full` as the
local/candidate baseline and attach public TURN JSON, soak summary, and DR records to the same
release evidence directory.
