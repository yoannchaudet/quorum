# Sessions (Human Intervention)

When the CO is blocked awaiting a human (see [state-machine](state-machine.md)), it
gathers input through a **named `copilot` Session** — a terminal conversation that can
be resumed later. This keeps HI in the same simple interface everywhere.

## Naming

A Session name ties it to a WI and the blocking state:

```
quorum/<wi-id>/<state>
# e.g. quorum/1234/IntakeReview
```

This makes the right Session trivial to find and resume for a given block.

## CLI behavior

When a WI enters a blocked state, the CLI:

1. Prints the current state and why it is blocked.
2. Creates (or reuses) the named Session and prints the **exact resume command**, e.g.:

   ```
   copilot --resume quorum/1234/IntakeReview
   ```
3. Polls the Session's outcome and resumes autonomous progress once the human is done.

## Tauri UX (future)

The UX offers a one-click action that launches a terminal (e.g. **ghostty**) running the
same resume command. Same Session, same interface — the UX only saves keystrokes.

## Why Sessions

- Resumable: a human can step away and come back.
- Uniform: identical mechanism for CLI and UX.
- Recoverable: the Session name is derived from persisted state, so it survives crashes
  (see [persistence](persistence.md)).
