ALTER TABLE fluvora_media_nodes
    ADD COLUMN IF NOT EXISTS ice_candidate text;

ALTER TABLE fluvora_media_nodes
    DROP CONSTRAINT IF EXISTS fluvora_media_nodes_ice_candidate_length;

ALTER TABLE fluvora_media_nodes
    ADD CONSTRAINT fluvora_media_nodes_ice_candidate_length
    CHECK (ice_candidate IS NULL OR length(ice_candidate) BETWEEN 1 AND 2048);
