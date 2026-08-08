# Observability

Quorum exposes the same typed Core data to the CLI and future frontends. It never relies
on parsing terminal prose to understand a work item.

## Live progress

`run`, `approve`, `reject`, and `answer` print concise timestamped activity to stderr:

- phase and state transitions;
- agent role/model, attempt, retry, completion, failure, and duration;
- convergence outcomes;
- implementation-round reservation, recovery, commit, or empty result;
- review verdicts and escalation;
- human-intervention blocking and terminal outcomes.

The start event is persisted and printed before a long agent invocation. Full prompts
and model output are not included. Pass global `--quiet` to suppress live rendering;
events are still persisted.

## Rich status

`quorum status <work-item>` is read-only and reports:

- work-item/repository identity and current state;
- latest activity and human-intervention instructions;
- planning iterations, planners, convergence, and Plan;
- implementation round states, commits, trees, and summaries;
- review verdicts/findings and failures;
- retained screenshots and browser/execution artifacts by implementation round;
- worktree path, branch, base, HEAD, and cleanliness;
- transition and activity history.

Default output uses snippets and the latest ten activity records. `--verbose` prints full
stored text and all activity. `--json` returns a versioned snapshot document for tools
and future UI clients.
The current document version is `4`; version 4 adds the human-approved execution
capability grant.

An unmatched old `agent_started` record is reported only as the latest known activity;
it is not proof that the process is still running.
