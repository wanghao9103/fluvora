# API service internal design

[简体中文](../API_SERVER_STRUCTURE.md) | [English](API_SERVER_STRUCTURE.md)

## 1. Design goals

The API service is the control-plane boundary for authentication, rooms, signaling, durable events,
and media orchestration. Its structure keeps transport, application policy, persistence, and
external clients separate so the process can be tested without hiding ownership in a monolithic
route module.

Primary constraints:

- bounded input and response sizes at the transport edge;
- authorization before state lookup or external side effects;
- explicit idempotency and optimistic revisions for mutations;
- no network await while a synchronous state lock is held;
- reconstructable in-memory registries with deterministic cleanup;
- one error model mapped consistently to public HTTP responses.

## 2. Directories and responsibilities

The service lives in `crates/services/api-server/src/`.

| Module | Responsibility |
|---|---|
| `main.rs` | Configuration, dependency construction, listeners, shutdown |
| `routes/` | HTTP/WSS extraction, content-type/size checks, response mapping |
| `services/` | Authorized use-case orchestration and cleanup |
| `models.rs` | API-owned request/response and bounded runtime state |
| `protocol.rs` | WHIP/WHEP and signaling protocol-session rules |
| `control_client.rs` | Authenticated media-node/worker/status calls |
| `runtime.rs` | Random identifiers, credentials, shutdown helpers |
| `validation.rs` | Shared route validation and supported media metadata |
| `error.rs` | Stable public error code/status mapping |

Large domains stay in focused route modules such as rooms, signaling, WebRTC, media tracks, VOD,
and live outputs. Cross-domain policy belongs in services rather than one route reaching into
another route's state.

## 3. Dependency direction

```text
main/router
   ↓
routes/transport
   ↓
application services
   ↓
domain + control-store contracts
   ↓
HTTP/store/process adapters
```

Routes may call application services and focused clients. Lower layers never import Axum route
types. External service payloads are translated at the client boundary.

## 4. Startup and shutdown

Startup validates all required secrets, URLs, CORS origins, state directories, capacity limits,
DTLS fingerprint source, and service tokens before accepting traffic. It then constructs the
control store, metrics, service clients, bounded registries, and router.

Shutdown stops admission, closes listeners, ends background tasks, and relies on explicit room/end
cleanup plus service timeouts for ephemeral media resources. Durable state is already committed in
the store before success is returned.

## 5. Request processing model

### 5.1 Room commands

```text
extract bounded request
→ authenticate bearer token and required scopes
→ validate room/participant/identifier fields
→ load current aggregate
→ apply domain command
→ transactionally persist snapshot + event + outbox/ledger
→ publish bounded live event
→ map result to response
```

Duplicate idempotency keys return the previously committed result. A stale expected revision returns
a conflict without applying partial state.

### 5.2 Media subscriptions

Publishing and subscription requests verify room mode, membership, role/scope, track metadata, codec
compatibility, and bounded resource limits. The API selects a healthy media node, provisions the
native transport, then records the session/track relationship. Failure rolls back any resource that
was already allocated.

### 5.3 Signaling and events

P2P signaling is an ordered bounded log addressed to a participant. WebSocket event access requires
a short-lived single-use ticket so bearer tokens are not placed in URLs. Initial replay and live
queues use the same record limits and terminate slow consumers instead of allowing unbounded memory.

## 6. State and concurrency ownership

| State | Owner | Synchronization |
|---|---|---|
| Durable rooms/events/idempotency/outbox | Control store/PostgreSQL | Transactions and revisions |
| Protocol sessions and media handles | API process | Bounded maps with narrow locks |
| WebSocket subscribers | Event hub | Bounded broadcast channels |
| Media transport | Media node | Session-local synchronization |
| Worker tasks and output files | Media worker/storage | Durable task state and leases |

Copy the minimum state needed for an external call, release the lock, await I/O, then reacquire and
verify the generation/revision before committing an in-memory update.

## 7. Persistence and consistency

Room mutations store the aggregate snapshot, ordered event, optional gift ledger row, idempotency
record, and outbox record in one PostgreSQL transaction. Outbox consumers use claim ownership,
expiry, and fencing to prevent stale acknowledgements. The in-memory store follows the same public
semantics for deterministic tests.

## 8. Error and security boundaries

Public failures use `{"code":"...","message":"..."}` with stable machine-readable codes. Internal
driver, URL, filesystem, and service details are not exposed. Authentication, scope, room binding,
payload size, safe URL rules, and identifier checks happen before expensive work. Redirects and
cross-origin behavior are explicit rather than inherited from permissive clients.

## 9. Adding a capability

1. Add or update the domain command and limits.
2. Add transactional store behavior and both backend tests when durable state changes.
3. Add a focused application service for multi-step orchestration.
4. Add the route with transport-only parsing and error mapping.
5. Update `API.md`, the SDK contract, affected SDKs, examples, and both documentation languages.
6. Add unit, integration, and browser/native tests at the appropriate boundary.

## 10. Style constraints

- Prefer explicit domain names over `utils`, `helpers`, or catch-all service modules.
- Keep route handlers short and linear.
- Use one canonical validation function for each shared invariant.
- Preserve async cancellation and do not block executor threads.
- Keep comments focused on invariants and protocol decisions, not restating code.
- Use the workspace formatter and lint configuration.

## 11. Automated validation

`scripts/check-architecture.ps1` checks dependency levels and focused-file limits.
`scripts/check-docs.ps1` checks bilingual pairs and local links. Rust format, Clippy, workspace tests,
SDK contracts, and release profiles enforce the remaining boundaries.
