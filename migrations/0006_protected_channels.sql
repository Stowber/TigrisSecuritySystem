CREATE TABLE IF NOT EXISTS tss.antinuke_protected_channels (
    guild_id BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    rotated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);