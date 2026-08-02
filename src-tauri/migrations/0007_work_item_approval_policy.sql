ALTER TABLE work_items
  ADD COLUMN require_plan_approval INTEGER NOT NULL DEFAULT 1
  CHECK(require_plan_approval IN (0, 1));
