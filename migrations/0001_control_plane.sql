CREATE TABLE IF NOT EXISTS fluvora_rooms (
    room_id text PRIMARY KEY,
    creation_command_id text NOT NULL UNIQUE,
    revision bigint NOT NULL CHECK (revision > 0),
    snapshot jsonb NOT NULL,
    ended boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (room_id ~ '^[0-9a-f]{32}$'),
    CHECK (creation_command_id ~ '^[0-9a-f]{32}$')
);

CREATE TABLE IF NOT EXISTS fluvora_room_events (
    room_id text NOT NULL REFERENCES fluvora_rooms(room_id) ON DELETE CASCADE,
    sequence bigint NOT NULL CHECK (sequence > 0),
    command_id text NOT NULL,
    event jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (room_id, sequence),
    UNIQUE (room_id, command_id),
    CHECK (command_id ~ '^[0-9a-f]{32}$')
);

CREATE TABLE IF NOT EXISTS fluvora_side_effects (
    room_id text NOT NULL REFERENCES fluvora_rooms(room_id) ON DELETE CASCADE,
    command_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (room_id, command_id),
    CHECK (command_id ~ '^[0-9a-f]{32}$')
);

CREATE TABLE IF NOT EXISTS fluvora_gift_ledger (
    transaction_id text PRIMARY KEY,
    room_id text NOT NULL REFERENCES fluvora_rooms(room_id),
    event_sequence bigint NOT NULL,
    sender_id text NOT NULL,
    recipient_id text NOT NULL,
    gift_id text NOT NULL,
    quantity integer NOT NULL CHECK (quantity > 0),
    unit_value bigint NOT NULL CHECK (unit_value >= 0),
    total_value numeric(39, 0) NOT NULL CHECK (total_value >= 0),
    currency char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    status text NOT NULL DEFAULT 'captured'
        CHECK (status IN ('captured', 'refunded', 'reversed')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (room_id, event_sequence)
);

CREATE TABLE IF NOT EXISTS fluvora_outbox (
    id bigserial PRIMARY KEY,
    aggregate_type text NOT NULL,
    aggregate_id text NOT NULL,
    aggregate_sequence bigint NOT NULL,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    available_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    lease_owner text,
    lease_until timestamptz,
    delivered_at timestamptz,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error text,
    UNIQUE (aggregate_type, aggregate_id, aggregate_sequence, event_type)
);

CREATE INDEX IF NOT EXISTS fluvora_outbox_pending_idx
    ON fluvora_outbox (available_at, id)
    WHERE delivered_at IS NULL;

CREATE TABLE IF NOT EXISTS fluvora_service_leases (
    resource_kind text NOT NULL,
    resource_id text NOT NULL,
    owner_id text NOT NULL,
    generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0),
    lease_until timestamptz NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (resource_kind, resource_id)
);

CREATE INDEX IF NOT EXISTS fluvora_service_leases_owner_idx
    ON fluvora_service_leases (owner_id, lease_until);

CREATE TABLE IF NOT EXISTS fluvora_media_nodes (
    node_id text PRIMARY KEY,
    region text NOT NULL,
    endpoint text NOT NULL,
    healthy boolean NOT NULL,
    draining boolean NOT NULL,
    rooms_used bigint NOT NULL CHECK (rooms_used >= 0),
    rooms_limit bigint NOT NULL CHECK (rooms_limit > 0),
    sessions_used bigint NOT NULL CHECK (sessions_used >= 0),
    sessions_limit bigint NOT NULL CHECK (sessions_limit > 0),
    publisher_tracks bigint NOT NULL CHECK (publisher_tracks >= 0),
    heartbeat_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS fluvora_media_nodes_placement_idx
    ON fluvora_media_nodes (region, healthy, draining, heartbeat_at);

CREATE TABLE IF NOT EXISTS fluvora_room_placements (
    room_id text PRIMARY KEY REFERENCES fluvora_rooms(room_id) ON DELETE CASCADE,
    node_id text NOT NULL REFERENCES fluvora_media_nodes(node_id),
    generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0),
    assigned_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
