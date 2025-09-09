-- 0005_antinuke.sql
-- Basic tables for antinuke incidents and snapshots.

CREATE SCHEMA IF NOT EXISTS tss;

CREATE TABLE IF NOT EXISTS tss.antinuke_guilds (
  guild_id BIGINT PRIMARY KEY,
  created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE IF NOT EXISTS tss.antinuke_incidents (
  id BIGSERIAL PRIMARY KEY,
  guild_id BIGINT NOT NULL REFERENCES tss.antinuke_guilds(guild_id) ON DELETE CASCADE,
  reason TEXT NOT NULL,
  created_at TIMESTAMPTZ DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_anti_incidents_guild ON tss.antinuke_incidents(guild_id);

CREATE TABLE IF NOT EXISTS tss.antinuke_snapshots (
  id BIGSERIAL PRIMARY KEY,
  incident_id BIGINT NOT NULL REFERENCES tss.antinuke_incidents(id) ON DELETE CASCADE,
  data JSONB NOT NULL,
  created_at TIMESTAMPTZ DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_anti_snapshots_incident ON tss.antinuke_snapshots(incident_id);

CREATE TABLE IF NOT EXISTS tss.antinuke_actions (
  id BIGSERIAL PRIMARY KEY,
  incident_id BIGINT NOT NULL REFERENCES tss.antinuke_incidents(id) ON DELETE CASCADE,
  actor_id BIGINT,
  kind TEXT NOT NULL,
  created_at TIMESTAMPTZ DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_anti_actions_incident ON tss.antinuke_actions(incident_id);