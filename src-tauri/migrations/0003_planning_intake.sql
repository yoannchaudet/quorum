CREATE TABLE app_settings (
  key TEXT PRIMARY KEY NOT NULL,
  value TEXT NOT NULL CHECK(length(trim(value)) > 0)
);

INSERT INTO app_settings (key, value) VALUES
  ('terminal_application', 'Ghostty.app'),
  (
    'terminal_arguments',
    '-W -na {terminalApplication} --args -e copilot -C "{repositoryPath}" --resume={sessionName}'
  );

CREATE TABLE work_items_v3 (
  id TEXT PRIMARY KEY NOT NULL,
  repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
  title TEXT NOT NULL CHECK(length(trim(title)) BETWEEN 1 AND 500),
  source_kind TEXT NOT NULL CHECK(source_kind IN ('inline_markdown', 'local_markdown', 'github_issue')),
  source_metadata_json TEXT NOT NULL CHECK(json_valid(source_metadata_json)),
  markdown_body TEXT NOT NULL,
  lifecycle_status TEXT NOT NULL CHECK(lifecycle_status IN ('open', 'completed', 'archived')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

INSERT INTO work_items_v3 (
  id,
  repository_id,
  title,
  source_kind,
  source_metadata_json,
  markdown_body,
  lifecycle_status,
  created_at,
  updated_at
)
SELECT
  id,
  repository_id,
  title,
  source_kind,
  '{"kind":"inline_markdown"}',
  markdown_body,
  lifecycle_status,
  created_at,
  updated_at
FROM work_items;

DROP TABLE work_items;
ALTER TABLE work_items_v3 RENAME TO work_items;
CREATE INDEX work_items_repository_idx ON work_items(repository_id, updated_at DESC);

CREATE TRIGGER work_items_normalized_source_immutable
BEFORE UPDATE OF source_kind, source_metadata_json, markdown_body ON work_items
FOR EACH ROW
WHEN
  OLD.source_kind <> NEW.source_kind
  OR OLD.source_metadata_json <> NEW.source_metadata_json
  OR OLD.markdown_body <> NEW.markdown_body
BEGIN
  SELECT RAISE(ABORT, 'normalized work item source is immutable');
END;

CREATE TABLE planning_runs (
  id TEXT PRIMARY KEY NOT NULL,
  work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK(status IN (
    'pending',
    'running',
    'waiting_for_answers',
    'synthesizing',
    'blocked',
    'succeeded',
    'failed',
    'cancelled'
  )),
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
CREATE INDEX planning_runs_work_item_idx
  ON planning_runs(work_item_id, created_at DESC);

CREATE TABLE planning_agents (
  id TEXT PRIMARY KEY NOT NULL,
  planning_run_id TEXT NOT NULL REFERENCES planning_runs(id) ON DELETE RESTRICT,
  role TEXT NOT NULL CHECK(role IN ('planner', 'synthesizer')),
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  model_id TEXT NOT NULL CHECK(length(trim(model_id)) > 0),
  session_name TEXT NOT NULL UNIQUE CHECK(length(trim(session_name)) > 0),
  status TEXT NOT NULL CHECK(status IN (
    'pending',
    'running',
    'blocked',
    'succeeded',
    'failed',
    'cancelled'
  )),
  error_code TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  UNIQUE(planning_run_id, role, ordinal),
  CHECK(role = 'planner' OR ordinal = 0),
  CHECK(
    (error_code IS NULL AND error_message IS NULL)
    OR (error_code IS NOT NULL AND error_message IS NOT NULL)
  )
);
CREATE INDEX planning_agents_run_idx
  ON planning_agents(planning_run_id, role, ordinal);

CREATE TABLE planning_artifacts (
  id TEXT PRIMARY KEY NOT NULL,
  planning_run_id TEXT NOT NULL REFERENCES planning_runs(id) ON DELETE RESTRICT,
  planning_agent_id TEXT REFERENCES planning_agents(id) ON DELETE RESTRICT,
  artifact_kind TEXT NOT NULL CHECK(artifact_kind IN (
    'planner_output',
    'synthesis_input',
    'synthesized_plan',
    'command_output'
  )),
  markdown_body TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX planning_artifacts_run_idx
  ON planning_artifacts(planning_run_id, artifact_kind, created_at);

CREATE TABLE planning_questions (
  id TEXT PRIMARY KEY NOT NULL,
  planning_run_id TEXT NOT NULL REFERENCES planning_runs(id) ON DELETE RESTRICT,
  planning_agent_id TEXT NOT NULL REFERENCES planning_agents(id) ON DELETE RESTRICT,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  prompt_markdown TEXT NOT NULL CHECK(length(trim(prompt_markdown)) > 0),
  status TEXT NOT NULL CHECK(status IN ('open', 'answered', 'dismissed')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(planning_agent_id, ordinal)
);
CREATE INDEX planning_questions_run_idx
  ON planning_questions(planning_run_id, status, ordinal);

CREATE TABLE planning_answers (
  id TEXT PRIMARY KEY NOT NULL,
  question_id TEXT NOT NULL UNIQUE REFERENCES planning_questions(id) ON DELETE RESTRICT,
  answer_markdown TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

ALTER TABLE plans
  ADD COLUMN planning_run_id TEXT REFERENCES planning_runs(id) ON DELETE RESTRICT;
ALTER TABLE plans
  ADD COLUMN edit_revision INTEGER NOT NULL DEFAULT 1 CHECK(edit_revision > 0);
ALTER TABLE plans
  ADD COLUMN approved_at TEXT;
ALTER TABLE plans
  ADD COLUMN queue_eligibility_key TEXT;
ALTER TABLE plans
  ADD COLUMN queue_eligible_at TEXT;

CREATE UNIQUE INDEX plans_planning_run_idx
  ON plans(planning_run_id)
  WHERE planning_run_id IS NOT NULL;
CREATE UNIQUE INDEX plans_queue_eligibility_key_idx
  ON plans(queue_eligibility_key)
  WHERE queue_eligibility_key IS NOT NULL;

ALTER TABLE queue_entries
  ADD COLUMN plan_id TEXT REFERENCES plans(id) ON DELETE RESTRICT;
ALTER TABLE queue_entries
  ADD COLUMN idempotency_key TEXT;

CREATE UNIQUE INDEX queue_entries_idempotency_key_idx
  ON queue_entries(idempotency_key)
  WHERE idempotency_key IS NOT NULL;
CREATE UNIQUE INDEX queue_entries_active_plan_idx
  ON queue_entries(plan_id)
  WHERE plan_id IS NOT NULL AND scheduling_status IN ('queued', 'paused');

CREATE TRIGGER queue_entries_require_eligible_plan_insert
BEFORE INSERT ON queue_entries
FOR EACH ROW
WHEN NEW.plan_id IS NOT NULL AND NOT EXISTS (
  SELECT 1
  FROM plans
  WHERE
    plans.id = NEW.plan_id
    AND plans.work_item_id = NEW.work_item_id
    AND plans.queue_eligibility_key = NEW.idempotency_key
    AND plans.queue_eligible_at IS NOT NULL
    AND (
      plans.approval_policy = 'not_required'
      OR plans.approval_status = 'approved'
    )
)
BEGIN
  SELECT RAISE(ABORT, 'queue entry requires an eligible plan');
END;

CREATE TRIGGER queue_entries_require_eligible_plan_update
BEFORE UPDATE OF work_item_id, plan_id, idempotency_key ON queue_entries
FOR EACH ROW
WHEN NEW.plan_id IS NOT NULL AND NOT EXISTS (
  SELECT 1
  FROM plans
  WHERE
    plans.id = NEW.plan_id
    AND plans.work_item_id = NEW.work_item_id
    AND plans.queue_eligibility_key = NEW.idempotency_key
    AND plans.queue_eligible_at IS NOT NULL
    AND (
      plans.approval_policy = 'not_required'
      OR plans.approval_status = 'approved'
    )
)
BEGIN
  SELECT RAISE(ABORT, 'queue entry requires an eligible plan');
END;
