-- 0002_warns.sql
-- System WARN – konfiguracja, pojedyncze sprawy (cases) i suma punktów.

-- Upewnij się, że schemat istnieje
CREATE SCHEMA IF NOT EXISTS tss;

-- Konfiguracja per-gildia (progi/escalacje)
CREATE TABLE IF NOT EXISTS tss.warn_config (
  guild_id       BIGINT PRIMARY KEY,
  decay_days     INTEGER NOT NULL DEFAULT 30,  -- po ilu dniach punkty naturalnie maleją
  timeout_pts    INTEGER NOT NULL DEFAULT 3,   -- próg timeoutu
  timeout_hours  INTEGER NOT NULL DEFAULT 12,  -- długość timeoutu przy osiągnięciu progu
  kick_pts       INTEGER NOT NULL DEFAULT 6,   -- próg kicka
  ban_pts        INTEGER NOT NULL DEFAULT 9,   -- próg bana
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Pojedynczy warn (sprawa)
CREATE TABLE IF NOT EXISTS tss.warn_cases (
  id            BIGSERIAL PRIMARY KEY,
  guild_id      BIGINT    NOT NULL,
  user_id       BIGINT    NOT NULL,
  moderator_id  BIGINT    NOT NULL,
  points        INTEGER   NOT NULL DEFAULT 1 CHECK (points > 0),
  reason        TEXT      NOT NULL,
  evidence      TEXT,                    -- URL / hash / krótki opis
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Przyspieszenie najczęstszych zapytań
CREATE INDEX IF NOT EXISTS idx_warn_cases_guild_user
  ON tss.warn_cases (guild_id, user_id);

CREATE INDEX IF NOT EXISTS idx_warn_cases_guild_created
  ON tss.warn_cases (guild_id, created_at DESC);

-- Suma punktów per użytkownik w gildii (cache/akumulator)
CREATE TABLE IF NOT EXISTS tss.warn_points (
  guild_id      BIGINT   NOT NULL,
  user_id       BIGINT   NOT NULL,
  total_points  INTEGER  NOT NULL DEFAULT 0,
  last_decay_at TIMESTAMPTZ,
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (guild_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_warn_points_guild
  ON tss.warn_points (guild_id);
