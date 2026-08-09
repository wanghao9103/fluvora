CREATE TABLE fluvora_token_revocations (
    subject_id CHAR(32) NOT NULL,
    token_nonce NUMERIC(20, 0) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 512),
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (subject_id, token_nonce),
    CHECK (subject_id ~ '^[0-9a-f]{32}$'),
    CHECK (token_nonce >= 0 AND token_nonce <= 18446744073709551615)
);

CREATE INDEX fluvora_token_revocations_expiry_idx
    ON fluvora_token_revocations (expires_at);
