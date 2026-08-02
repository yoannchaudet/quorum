CREATE TABLE model_assignments (
  role TEXT NOT NULL CHECK(role IN ('planner', 'implementation', 'adversary')),
  position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 2),
  model_id TEXT NOT NULL CHECK(length(trim(model_id)) > 0),
  PRIMARY KEY(role, position),
  CHECK(role = 'planner' OR position = 0)
);

INSERT INTO model_assignments (role, position, model_id) VALUES
  ('planner', 0, 'gpt-5.6-sol'),
  ('planner', 1, 'claude-opus-5'),
  ('implementation', 0, 'gpt-5.6-sol'),
  ('adversary', 0, 'claude-opus-5');
