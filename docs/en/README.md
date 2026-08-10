# Fluvora documentation

[简体中文](../README.md) | [English](README.md)

This is the entry point for repository documentation. Design, API, operations, and acceptance
material is separated by responsibility. Each fact should have one primary source; other documents
link to it to prevent gradual divergence.

## Recommended reading paths

### First time in the codebase

1. [Codebase guide](CODEBASE.md): directories, crate responsibilities, and code placement rules.
2. [Architecture](ARCHITECTURE.md): runtime topology, media modes, security, and recovery.
3. [Layering rules](LAYERS.md): workspace dependency direction and automated gates.
4. [API service design](API_SERVER_STRUCTURE.md): control-plane modules, call paths, and state ownership.

### Integrating an SDK or public API

1. [SDK integration guide](SDK_INTEGRATION.md): installation, authentication, media, errors,
   cleanup, and troubleshooting for all five targets.
2. [Public API](API.md): HTTP, WebSocket, WHIP/WHEP, and media-control interfaces.
3. [SDK demo specification](SDK_DEMOS.md): runnable examples and platform capability coverage.
4. [Independent SDK releases](SDK_RELEASES.md): version sources, changelog boundaries, tags, and candidates.
5. [`sdk-contract-v1.json`](../sdk-contract-v1.json): machine-verifiable cross-platform contract.
6. [`sdk-demo-contract-v1.json`](../sdk-demo-contract-v1.json): machine-verifiable demo coverage.

### Deployment and release

1. [Production acceptance](PRODUCTION_ACCEPTANCE.md): required gates and release evidence.
2. [Operations runbook](RUNBOOK.md): startup, observability, incident diagnosis, and recovery.
3. [Development plan](DEVELOPMENT_PLAN.md): milestones, roles, and delivery cadence.

## Document responsibilities

| Document | Primary question | Update trigger |
|---|---|---|
| `ARCHITECTURE.md` | Which processes exist and how do control and media planes cooperate? | Topology, protocol boundary, or ownership changes |
| `LAYERS.md` | Which crates may depend on which layers? | Workspace crate or dependency-level changes |
| `CODEBASE.md` | Where does code live and where should new code go? | Directory, crate, or primary-module changes |
| `API_SERVER_STRUCTURE.md` | How is the API service layered internally? | Responsibility, call-path, concurrency, or gate changes |
| `API.md` | Which public interfaces can clients call? | Route, field, status, or limit changes |
| `SDK_INTEGRATION.md` | How do all five SDK targets install, connect, fail, and clean up? | Constructor, public method, or platform-boundary changes |
| `SDK_DEMOS.md` | How are SDK capabilities demonstrated? | Public method or runnable-example changes |
| `SDK_RELEASES.md` | How do SDKs version, build, and track changes independently? | SDK version, tag, or release-process changes |
| `PRODUCTION_ACCEPTANCE.md` | Which checks are mandatory before release? | Gate, capacity, or security requirement changes |
| `RUNBOOK.md` | How are production faults detected and handled? | Configuration, metric, alert, or recovery changes |
| `DEVELOPMENT_PLAN.md` | How is the project delivered in phases? | Milestone, staffing, or scope changes |

## Source-of-truth order

When descriptions conflict, resolve them in this order:

1. executable contracts, database migrations, and source-code boundaries;
2. `API.md`, `ARCHITECTURE.md`, and focused design documents;
3. `CODEBASE.md`, README files, and examples;
4. target-state descriptions in the development plan.

Do not fix a conflict only in a lower-priority document. Confirm the intended design first, then
synchronize code, contracts, and every affected document in both languages.

## Maintenance rules

- New public routes update `API.md`, the SDK contract, and affected SDKs.
- New services or workspace crates update `ARCHITECTURE.md`, `LAYERS.md`, and `CODEBASE.md`.
- API layering changes update `API_SERVER_STRUCTURE.md` and architecture gates.
- Configuration, port, metric, or recovery changes update `RUNBOOK.md`.
- English and Chinese documents are updated in the same change.
- Full release evidence is stored in `artifacts/release-gates-*/release-gates.json`.
- Markdown uses UTF-8, LF, and repository-relative links as defined by `.editorconfig`.

After documentation changes, run at least:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-docs.ps1
```
