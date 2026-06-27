-- Migration: 0001_create_tasks.sql

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE tasks (
    id              UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id         UUID        NOT NULL,
    parent_id       UUID        REFERENCES tasks(id) ON DELETE SET NULL,
    title           VARCHAR(500) NOT NULL,
    description     TEXT        NOT NULL DEFAULT '',
    status          VARCHAR(50) NOT NULL DEFAULT 'todo'
                    CHECK (status IN ('todo', 'in_progress', 'done', 'cancelled')),
    estimated_mins  INTEGER     NOT NULL DEFAULT 0 CHECK (estimated_mins >= 0),
    spent_mins      INTEGER     NOT NULL DEFAULT 0 CHECK (spent_mins >= 0),
    is_deleted      BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for common query patterns
CREATE INDEX idx_tasks_user_id        ON tasks(user_id) WHERE is_deleted = FALSE;
CREATE INDEX idx_tasks_parent_id      ON tasks(parent_id) WHERE is_deleted = FALSE;
CREATE INDEX idx_tasks_user_parent    ON tasks(user_id, parent_id) WHERE is_deleted = FALSE;
CREATE INDEX idx_tasks_status         ON tasks(status) WHERE is_deleted = FALSE;

-- Auto-update updated_at on row change
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER tasks_updated_at
    BEFORE UPDATE ON tasks
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
