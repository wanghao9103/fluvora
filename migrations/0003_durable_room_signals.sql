ALTER TABLE fluvora_rooms
    ADD COLUMN signal_sequence BIGINT NOT NULL DEFAULT 0
        CHECK (signal_sequence >= 0);

CREATE TABLE fluvora_room_signals (
    room_id TEXT NOT NULL REFERENCES fluvora_rooms(room_id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    command_id TEXT NOT NULL,
    from_id TEXT NOT NULL,
    to_id TEXT,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    payload JSONB NOT NULL,
    timestamp_millis BIGINT NOT NULL CHECK (timestamp_millis >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (room_id, sequence),
    UNIQUE (room_id, command_id)
);

CREATE INDEX fluvora_room_signals_recipient_idx
    ON fluvora_room_signals (room_id, to_id, sequence);
