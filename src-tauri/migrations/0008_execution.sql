CREATE TABLE execution_runs (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
  queue_entry_id TEXT NOT NULL UNIQUE REFERENCES queue_entries(id) ON DELETE RESTRICT,
  source_repository_path TEXT NOT NULL,
  base_commit TEXT,
  branch_name TEXT NOT NULL UNIQUE,
  worktree_path TEXT NOT NULL UNIQUE,
  ownership_token TEXT NOT NULL UNIQUE,
  ownership_claimed_at TEXT,
  ownership_verified_at TEXT,
  git_metadata_json TEXT CHECK(
    git_metadata_json IS NULL OR json_valid(git_metadata_json)
  ),
  copilot_program TEXT,
  builder_session_id TEXT NOT NULL UNIQUE,
  builder_session_name TEXT NOT NULL UNIQUE,
  builder_model TEXT NOT NULL,
  reviewer_session_id TEXT NOT NULL UNIQUE,
  reviewer_session_name TEXT NOT NULL UNIQUE,
  reviewer_model TEXT NOT NULL,
  verification_program TEXT,
  verification_args_json TEXT CHECK(
    verification_args_json IS NULL OR json_valid(verification_args_json)
  ),
  latest_verification_command_id TEXT,
  verified_state_digest TEXT CHECK(
    verified_state_digest IS NULL OR length(verified_state_digest) = 64
  ),
  reviewed_state_digest TEXT CHECK(
    reviewed_state_digest IS NULL OR length(reviewed_state_digest) = 64
  ),
  status TEXT NOT NULL CHECK(status IN (
    'starting',
    'building',
    'verifying',
    'reviewing',
    'remediating',
    'cancelling',
    'blocked',
    'ready',
    'failed',
    'cancelled'
  )),
  current_step TEXT NOT NULL CHECK(current_step IN (
    'preparing',
    'building',
    'verifying',
    'reviewing',
    'remediating',
    'complete'
  )),
  iteration INTEGER NOT NULL DEFAULT 0 CHECK(iteration BETWEEN 0 AND 3),
  max_iterations INTEGER NOT NULL DEFAULT 3 CHECK(max_iterations BETWEEN 1 AND 3),
  idempotency_key TEXT NOT NULL UNIQUE CHECK(length(trim(idempotency_key)) > 0),
  builder_session_started_at TEXT,
  builder_session_state TEXT NOT NULL DEFAULT 'not_started' CHECK(
    builder_session_state IN ('not_started', 'launching', 'resumable')
  ),
  builder_completed_at TEXT,
  reviewer_session_started_at TEXT,
  reviewer_session_state TEXT NOT NULL DEFAULT 'not_started' CHECK(
    reviewer_session_state IN ('not_started', 'launching', 'resumable')
  ),
  pending_builder_prompt TEXT,
  remediation_diff_hash TEXT,
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

CREATE INDEX execution_runs_status_idx ON execution_runs(status, updated_at DESC);

CREATE TABLE execution_attempts (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL REFERENCES execution_runs(run_id) ON DELETE RESTRICT,
  number INTEGER NOT NULL CHECK(number > 0),
  reason TEXT NOT NULL CHECK(reason IN ('start', 'resume')),
  status TEXT NOT NULL CHECK(status IN (
    'running',
    'succeeded',
    'blocked',
    'failed',
    'cancelled',
    'interrupted'
  )),
  error_code TEXT,
  error_message TEXT,
  started_at TEXT NOT NULL,
  completed_at TEXT,
  UNIQUE(run_id, number),
  CHECK(
    (error_code IS NULL AND error_message IS NULL)
    OR (error_code IS NOT NULL AND error_message IS NOT NULL)
  )
);
CREATE INDEX execution_attempts_run_idx
  ON execution_attempts(run_id, number DESC);

CREATE TABLE execution_commands (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL REFERENCES execution_runs(run_id) ON DELETE RESTRICT,
  execution_attempt_id TEXT NOT NULL REFERENCES execution_attempts(id) ON DELETE RESTRICT,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  phase TEXT NOT NULL CHECK(phase IN (
    'preparing',
    'building',
    'verifying',
    'reviewing',
    'remediating'
  )),
  program TEXT NOT NULL CHECK(length(trim(program)) > 0),
  args_json TEXT NOT NULL CHECK(json_valid(args_json)),
  cwd TEXT NOT NULL CHECK(length(trim(cwd)) > 0),
  status TEXT NOT NULL CHECK(status IN (
    'running',
    'succeeded',
    'failed',
    'cancelled',
    'interrupted'
  )),
  exit_code INTEGER,
  output_truncated INTEGER NOT NULL DEFAULT 0 CHECK(output_truncated IN (0, 1)),
  started_at TEXT NOT NULL,
  completed_at TEXT,
  UNIQUE(execution_attempt_id, ordinal)
);
CREATE INDEX execution_commands_run_idx
  ON execution_commands(run_id, started_at DESC);

CREATE TABLE execution_logs (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL REFERENCES execution_runs(run_id) ON DELETE RESTRICT,
  command_id TEXT NOT NULL REFERENCES execution_commands(id) ON DELETE RESTRICT,
  sequence INTEGER NOT NULL CHECK(sequence >= 0),
  stream TEXT NOT NULL CHECK(stream IN ('stdout', 'stderr', 'system')),
  text TEXT NOT NULL CHECK(length(text) <= 16384),
  truncated INTEGER NOT NULL DEFAULT 0 CHECK(truncated IN (0, 1)),
  created_at TEXT NOT NULL,
  UNIQUE(command_id, sequence)
);
CREATE INDEX execution_logs_run_idx
  ON execution_logs(run_id, created_at DESC, sequence DESC);

CREATE TABLE execution_reviews (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL REFERENCES execution_runs(run_id) ON DELETE RESTRICT,
  iteration INTEGER NOT NULL CHECK(iteration BETWEEN 0 AND 3),
  command_id TEXT NOT NULL REFERENCES execution_commands(id) ON DELETE RESTRICT,
  summary TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(run_id, iteration)
);

CREATE TABLE execution_findings (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL REFERENCES execution_runs(run_id) ON DELETE RESTRICT,
  external_id TEXT NOT NULL,
  severity TEXT NOT NULL CHECK(severity IN ('blocking', 'warning')),
  title TEXT NOT NULL CHECK(length(trim(title)) > 0),
  body TEXT NOT NULL CHECK(length(trim(body)) > 0),
  path TEXT,
  line INTEGER CHECK(line IS NULL OR line > 0),
  status TEXT NOT NULL CHECK(status IN ('open', 'fixed', 'resolved')),
  disposition_note TEXT,
  first_seen_iteration INTEGER NOT NULL CHECK(first_seen_iteration BETWEEN 0 AND 3),
  last_seen_iteration INTEGER NOT NULL CHECK(last_seen_iteration BETWEEN 0 AND 3),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  resolved_at TEXT,
  UNIQUE(run_id, external_id),
  CHECK(
    status <> 'resolved'
    OR (disposition_note IS NOT NULL AND length(trim(disposition_note)) > 0)
  )
);
CREATE INDEX execution_findings_run_idx
  ON execution_findings(run_id, severity, status, updated_at DESC);
