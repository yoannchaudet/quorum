CREATE TABLE repositories (
  id TEXT PRIMARY KEY NOT NULL,
  root_path TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL CHECK(length(trim(display_name)) > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT
);
CREATE INDEX repositories_active_idx ON repositories(archived_at, display_name);

CREATE TABLE work_items (
  id TEXT PRIMARY KEY NOT NULL,
  repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
  title TEXT NOT NULL CHECK(length(trim(title)) BETWEEN 1 AND 500),
  source_kind TEXT NOT NULL CHECK(source_kind IN ('inline_markdown')),
  markdown_body TEXT NOT NULL,
  lifecycle_status TEXT NOT NULL CHECK(lifecycle_status IN ('open', 'completed', 'archived')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX work_items_repository_idx ON work_items(repository_id, updated_at DESC);

CREATE TABLE plans (
  id TEXT PRIMARY KEY NOT NULL,
  work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE RESTRICT,
  revision INTEGER NOT NULL CHECK(revision > 0),
  markdown_body TEXT NOT NULL,
  approval_policy TEXT NOT NULL CHECK(approval_policy IN ('not_required', 'required')),
  approval_status TEXT NOT NULL CHECK(approval_status IN ('draft', 'pending', 'approved', 'rejected')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(work_item_id, revision)
);

CREATE TABLE runs (
  id TEXT PRIMARY KEY NOT NULL,
  work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE RESTRICT,
  plan_id TEXT REFERENCES plans(id) ON DELETE RESTRICT,
  phase TEXT NOT NULL CHECK(phase IN ('planning', 'building', 'reviewing', 'delivery')),
  outcome TEXT CHECK(outcome IN ('pending', 'running', 'succeeded', 'failed', 'blocked', 'cancelled')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX runs_work_item_idx ON runs(work_item_id, created_at DESC);

CREATE TABLE phase_events (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
  sequence INTEGER NOT NULL CHECK(sequence >= 0),
  event_kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(run_id, sequence)
);
CREATE INDEX phase_events_run_order_idx ON phase_events(run_id, sequence);

CREATE TABLE queue_entries (
  id TEXT PRIMARY KEY NOT NULL,
  work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE RESTRICT,
  run_id TEXT REFERENCES runs(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK(position >= 0),
  scheduling_status TEXT NOT NULL CHECK(scheduling_status IN ('queued', 'paused', 'cancelled')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(position)
);
CREATE INDEX queue_entries_status_position_idx ON queue_entries(scheduling_status, position);
