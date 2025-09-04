-- Używamy schematu tss
CREATE SCHEMA IF NOT EXISTS tss;


-- Serwery (gildie)
CREATE TABLE IF NOT EXISTS tss.guilds (
guild_id BIGINT PRIMARY KEY,
name TEXT NOT NULL,
modlog_channel BIGINT,
admin_role_ids BIGINT[] DEFAULT '{}',
moderator_role_ids BIGINT[] DEFAULT '{}',
created_at TIMESTAMPTZ DEFAULT now(),
updated_at TIMESTAMPTZ DEFAULT now()
);


-- Rejestr zasobów (klucz logiczny -> ID Discord)
CREATE TABLE IF NOT EXISTS tss.resource_registry (
guild_id BIGINT NOT NULL,
key TEXT NOT NULL,
kind TEXT NOT NULL CHECK (kind IN ('ROLE','CHANNEL','WEBHOOK','EMOJI','CATEGORY')),
discord_id BIGINT NOT NULL,
meta JSONB DEFAULT '{}'::jsonb,
updated_at TIMESTAMPTZ DEFAULT now(),
PRIMARY KEY (guild_id, key)
);


-- Capabilities per rola
CREATE TABLE IF NOT EXISTS tss.role_capabilities (
guild_id BIGINT NOT NULL,
role_id BIGINT NOT NULL,
capability TEXT NOT NULL,
granted_at TIMESTAMPTZ DEFAULT now(),
PRIMARY KEY (guild_id, role_id, capability)
);


-- Log audytu (skrócona wersja na start)
CREATE TABLE IF NOT EXISTS tss.audit_log (
id BIGSERIAL PRIMARY KEY,
guild_id BIGINT NOT NULL,
actor_id BIGINT,
event TEXT NOT NULL,
payload JSONB,
created_at TIMESTAMPTZ DEFAULT now()
);