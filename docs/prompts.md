# Prompts

Every prompt Quorum gives an agent (PL, IM, RV, and CO merge/convergence prompts) is a
**reviewable markdown file**. Prompts are code: versioned, diffable, human-readable.

## Location

```
prompts/
  planner.md
  merge.md
  implementer.md
  reviewer.md
  intake-questions.md
```

## Frontmatter

Each file starts with a small custom frontmatter block so we can tell files apart at a
glance. It is for humans and tooling — format is ours, kept minimal.

```markdown
---
id: planner
role: PL
model-target: planners     # which config slot(s) this prompt is used for
version: 1
purpose: Produce a candidate plan for a WI in isolation.
---

# Body: the actual prompt in plain markdown.
```

### Fields

| Field | Meaning |
|-------|---------|
| `id` | Unique, matches the filename stem. |
| `role` | One of CO / PL / IM / RV (see [glossary](glossary.md)). |
| `model-target` | Config key(s) this prompt applies to (see [config](config.md)). |
| `version` | Integer, bumped on any material change. |
| `purpose` | One line, what this prompt is for. |

## Rules

- One prompt per file; filename stem == `id`.
- Bump `version` on any change to the body.
- Keep the body plain markdown so diffs are readable and reviews are easy.
