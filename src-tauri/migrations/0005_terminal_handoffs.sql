CREATE TABLE terminal_handoffs (
  id TEXT PRIMARY KEY NOT NULL,
  work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE RESTRICT,
  planning_run_id TEXT NOT NULL REFERENCES planning_runs(id) ON DELETE RESTRICT,
  planning_agent_id TEXT NOT NULL REFERENCES planning_agents(id) ON DELETE RESTRICT,
  session_name TEXT NOT NULL CHECK(length(trim(session_name)) > 0),
  idempotency_key TEXT NOT NULL UNIQUE CHECK(length(trim(idempotency_key)) > 0),
  status TEXT NOT NULL CHECK(status IN (
    'launching',
    'awaiting_manual_reconcile',
    'reconciling',
    'reconciled',
    'launch_failed',
    'reconcile_failed'
  )),
  completion_observable INTEGER CHECK(completion_observable IN (0, 1)),
  error_code TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  CHECK(
    (error_code IS NULL AND error_message IS NULL)
    OR (error_code IS NOT NULL AND error_message IS NOT NULL)
  )
);

CREATE INDEX terminal_handoffs_work_item_idx
  ON terminal_handoffs(work_item_id, created_at DESC);
CREATE INDEX terminal_handoffs_agent_idx
  ON terminal_handoffs(planning_agent_id, created_at DESC);
