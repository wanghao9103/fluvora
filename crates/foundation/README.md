# Foundation

Shared vocabulary and dependency-light utilities:

- `domain`: rooms, members, commands, policies, and domain events.
- `protocol`: versioned application wire envelopes.
- `bytes-codec`: bounded network-byte-order readers and writers.
- `observability`: reusable metrics and health models.

Code here must not know about HTTP routes, databases, deployable services, or
external process orchestration.
