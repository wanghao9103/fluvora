# Persistence migrations

Production mode uses PostgreSQL migrations embedded by `fluvora-control-store`. Migration versions
are strictly increasing and their SHA-256 checksums are recorded under an advisory transaction
lock. Never edit an applied migration; add the next numbered file.

`0001_control_plane.sql` creates:

- compare-and-swap room snapshots and immutable room events;
- transactional outbox records;
- globally unique gift-ledger transactions;
- cross-replica side-effect idempotency;
- fenced service/job leases;
- media-node capacity and room-placement records.

The API keeps the bounded file backend only for local single-instance development. Production
deployments set `FLUVORA_DATABASE_URL`, run migrations during startup, and must not fall back to a
shared file volume.
