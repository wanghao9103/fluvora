# Fluvora deployment

## Compose

```bash
cp deploy/compose/.env.example deploy/compose/.env
# Replace secrets and FLUVORA_PUBLIC_IP.
docker compose --env-file deploy/compose/.env -f deploy/compose/compose.yaml up --build -d
```

The Compose stack starts the API, status service, event dispatcher, media node, worker, media
gateway and TURN server, creates an ECDSA DTLS/TURN certificate, persists control and media data,
exposes the fixed TURN relay UDP range, and provisions PostgreSQL, NATS JetStream, Prometheus,
Alertmanager and Grafana.

## Kubernetes

1. Replace the image in `deploy/kubernetes/base/kustomization.yaml`.
2. Patch `REPLACE_PUBLIC_IP`, domains, region and storage class.
3. Create `fluvora-secrets` from `secret.example.yaml` without committing real values.
4. Create `fluvora-dtls`:

```bash
kubectl create namespace fluvora
kubectl -n fluvora create secret tls fluvora-dtls --cert=dtls.crt --key=dtls.key
kubectl apply -f deploy/kubernetes/base/secret.yaml
kubectl apply -k deploy/kubernetes/base
```

The API, status service, event dispatcher and worker are stateless or lease/fencing coordinated and
start with multiple replicas; their PDBs and HPAs are in `availability.yaml`. PostgreSQL is the
transactional source of truth, NATS JetStream transports the outbox, and object media is stored
outside pod filesystems. `NetworkPolicy` denies unspecified ingress.

Media-node and TURN run as one host-networked `DaemonSet` pod per Kubernetes node because WebRTC and
the TURN relay range require direct UDP reachability. The downward API supplies a stable node ID,
per-node control endpoint and host candidate. PostgreSQL heartbeats drive capacity-aware room
placement; `draining`, heartbeat expiry and fencing generations prevent a terminating or stale
owner from accepting new work. SIGTERM marks the instance draining before its listeners finish
graceful shutdown. PDBs limit voluntary disruption to one media/TURN node at a time.

`status.hostIP` is a valid advertised address only when worker nodes are publicly routable or have a
one-to-one NAT mapping. Otherwise patch `FLUVORA_MEDIA_ADVERTISE_CANDIDATE` and
`FLUVORA_TURN_ADVERTISED_IP` from a cloud load-balancer/host mapping controller. Publish all TURN
nodes through multi-A/AAAA DNS, anycast, or a UDP/TCP/TLS load balancer and set `FLUVORA_ICE_URLS`
to that stable public name. Every node may reuse the relay port range because each has a distinct
host IP, but the complete range must be opened on every node firewall.

Gateway replicas use the shared object-store path for durable assets. If local live storage is
selected instead, use shard-aware routing so every request for one stream reaches its owning
gateway.

Public deployments should keep `FLUVORA_TURN_ALLOW_PRIVATE_PEERS=false`; this prevents TURN from
relaying to loopback, link-local, carrier-grade NAT, and private subnets. Set it to `true` only for
an intentionally private topology. `FLUVORA_TURN_MAX_RELAY_BYTES_PER_SECOND` applies a combined
bidirectional per-allocation token bucket with a two-second burst.

## Monitoring

Prometheus rules are in `monitoring/alerts.yml`; Grafana provisioning and the overview dashboard are
under `monitoring/grafana`. Configure a real Alertmanager receiver before production. Release,
rollback, backup/restore and failure drills are documented in `docs/RUNBOOK.md`; mandatory evidence
is listed in `docs/PRODUCTION_ACCEPTANCE.md`.
