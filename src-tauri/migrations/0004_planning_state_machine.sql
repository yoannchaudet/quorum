ALTER TABLE planning_runs ADD COLUMN idempotency_key TEXT;
CREATE UNIQUE INDEX planning_runs_idempotency_key_idx
  ON planning_runs(idempotency_key)
  WHERE idempotency_key IS NOT NULL;

ALTER TABLE planning_agents ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0);
ALTER TABLE planning_agents ADD COLUMN started_at TEXT;

ALTER TABLE planning_questions ADD COLUMN external_id TEXT NOT NULL DEFAULT '';
CREATE INDEX planning_questions_agent_external_idx
  ON planning_questions(planning_agent_id, external_id);

ALTER TABLE planning_artifacts ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0);
ALTER TABLE planning_artifacts ADD COLUMN sequence INTEGER NOT NULL DEFAULT 0 CHECK(sequence >= 0);

CREATE TABLE planning_agent_events (
  id TEXT PRIMARY KEY NOT NULL,
  planning_run_id TEXT NOT NULL REFERENCES planning_runs(id) ON DELETE RESTRICT,
  planning_agent_id TEXT NOT NULL REFERENCES planning_agents(id) ON DELETE RESTRICT,
  attempt INTEGER NOT NULL CHECK(attempt > 0),
  sequence INTEGER NOT NULL CHECK(sequence >= 0),
  event_kind TEXT,
  payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
  created_at TEXT NOT NULL,
  UNIQUE(planning_agent_id, attempt, sequence)
);
CREATE INDEX planning_agent_events_run_order_idx
  ON planning_agent_events(planning_run_id, planning_agent_id, attempt, sequence);
