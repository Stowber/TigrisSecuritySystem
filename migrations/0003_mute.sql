-- 0003_mute.sql
-- Mute system: konfiguracja i historia wyciszeń

-- Upewnij się, że schemat istnieje
CREATE SCHEMA IF NOT EXISTS tss;

-- Konfiguracja per-gildia (JSONB)
CREATE TABLE IF NOT EXISTS tss.mute_config (
  guild_id BIGINT PRIMARY KEY,
  cfg      JSONB NOT NULL DEFAULT '{}'::jsonb
);

-- Na wypadek starej wersji bez kolumny cfg
ALTER TABLE tss.mute_config
  ADD COLUMN IF NOT EXISTS cfg JSONB NOT NULL DEFAULT '{}'::jsonb;

-- Historia mute (case’y)
CREATE TABLE IF NOT EXISTS tss.mute_cases (
  id            BIGSERIAL PRIMARY KEY,
  guild_id      BIGINT       NOT NULL,
  user_id       BIGINT       NOT NULL,
  moderator_id  BIGINT       NOT NULL,
  reason        TEXT         NOT NULL,
  evidence      TEXT         NULL,
  created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
  until         TIMESTAMPTZ  NULL,
  unmuted_at    TIMESTAMPTZ  NULL,
  unmuted_by    BIGINT       NULL,
  unmute_reason TEXT         NULL,
  method        TEXT         NOT NULL DEFAULT 'role', -- 'role' | 'timeout'
  role_id       BIGINT       NULL
);

-- Idempotentne dodanie brakujących kolumn (gdy tabela istniała wcześniej)
ALTER TABLE tss.mute_cases
  ADD COLUMN IF NOT EXISTS until         TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS unmuted_at    TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS unmuted_by    BIGINT      NULL,
  ADD COLUMN IF NOT EXISTS unmute_reason TEXT        NULL,
  ADD COLUMN IF NOT EXISTS method        TEXT        NOT NULL DEFAULT 'role',
  ADD COLUMN IF NOT EXISTS role_id       BIGINT      NULL;

-- Indeksy pod najczęstsze zapytania
CREATE INDEX IF NOT EXISTS idx_mute_cases_gid_uid_created
  ON tss.mute_cases (guild_id, user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_mute_cases_gid_until
  ON tss.mute_cases (guild_id, until);
