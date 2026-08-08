# Sessions (Human Intervention)

When the Coordinator is blocked awaiting a human, it
gathers input through a **named `copilot` Session** — a terminal conversation that can
be resumed later. This keeps human intervention in the same simple interface everywhere.

## Naming

A Session name ties it to a work item and the blocking state:

```
quorum/<work-item-slug>/<state>
# e.g. quorum/1234/IntakeReview
```

This makes the right Session trivial to find and resume for a given block.

## CLI behavior

When a work item enters a blocked state, the CLI:

1. Prints the current state and why it is blocked.
2. Records the deterministic session name (see above) and surfaces it, with the
   commands to name/resume the `copilot` session:
   - First time, start a session and name it: run `copilot`, then `/rename quorum/1234/IntakeReview`.
   - Later, resume it: `copilot --resume quorum/1234/IntakeReview`.

   > `copilot` has no non-interactive flag to create a session with a chosen
   > name, so the human assigns the name once via `/rename`; Quorum supplies the
   > name to use so it stays consistent for the block.
3. The human resolves the block with the state-specific command:
   - `quorum intake answer <work-item> ...`;
   - `quorum plan approve|reject <work-item>`;
   - `quorum implementation approve|reject <work-item>`;
   - `quorum work-item abandon <work-item>`.

   Rejection accepts optional positional feedback or `--file` and carries that
   guidance into the next planning or implementation pass.

## Tauri UX (future)

The UX offers a one-click action that launches a terminal (e.g. **ghostty**) running the
same resume command. Same Session, same interface — the UX only saves keystrokes.

## Why Sessions

- Resumable: a human can step away and come back.
- Uniform: identical mechanism for CLI and UX.
- Recoverable: the Session name is derived from persisted state, so it survives crashes
  (see [persistence](persistence.md)).
