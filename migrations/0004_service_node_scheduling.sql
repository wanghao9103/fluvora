CREATE TABLE fluvora_service_nodes (
    node_id TEXT PRIMARY KEY,
    service_kind TEXT NOT NULL,
    region TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    healthy BOOLEAN NOT NULL,
    draining BOOLEAN NOT NULL,
    jobs_used BIGINT NOT NULL CHECK (jobs_used >= 0),
    jobs_limit BIGINT NOT NULL CHECK (jobs_limit > 0 AND jobs_used <= jobs_limit),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    metadata JSONB NOT NULL DEFAULT '{}'::JSONB
);

CREATE INDEX fluvora_service_nodes_scheduler_idx
    ON fluvora_service_nodes
        (service_kind, region, healthy, draining, heartbeat_at DESC);

CREATE TABLE fluvora_service_resource_placements (
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES fluvora_service_nodes(node_id),
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation > 0),
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (resource_kind, resource_id)
);

CREATE INDEX fluvora_service_resource_placements_node_idx
    ON fluvora_service_resource_placements (node_id);
