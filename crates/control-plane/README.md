# Control plane

Shared coordination used by deployable services:

- `auth`: access-token encoding and validation.
- `control-store`: durable PostgreSQL state and migrations.
- `event-dispatcher`: outbox and NATS event delivery.
- `status-service`: service health and capacity aggregation.
- `status-client`: heartbeat reporting client.

Protocol packet handling and media processing do not belong in this directory.
