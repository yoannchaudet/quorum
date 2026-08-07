# Work Items

A WI is a **local markdown file**. It is the single input Quorum processes.

## Sources

| Source | How |
|--------|-----|
| Local | Point the CLI at an existing `.md` file. |
| GitHub | The CLI pulls a GitHub issue and writes it to a local `.md` file (body + metadata). The repo defaults to the one for the current working directory; it can also be passed on the CLI. From then on it is treated as a local WI. |

Pulling from GitHub is a one-time import: Quorum operates on the local copy so it can run unattended and offline.

## Images

Images are first-class — they will be used extensively.

- Markdown embeds images GitHub-style: `![alt](path-or-url)`.
- **Local images**: relative paths are resolved against the WI file's directory.
- **Remote images** (e.g. GitHub-hosted attachments): downloaded during `Intake` and
  rewritten to local paths so the WI is self-contained and reproducible.
- Downloaded assets are stored alongside the WI's on-disk state (see [persistence](persistence.md)).

## On-disk shape

```
<state-dir>/<wi-id>/
  quorum.db           # WI text + all CO state (see persistence.md)
  assets/             # downloaded/embedded images
  implementation/     # IM output
```

The WI's normalized markdown is stored in `quorum.db`; only binary images live under
`assets/`.

## Validation (during Intake)

- File exists and is valid UTF-8 markdown.
- All embedded image references resolve (local or successfully downloaded).
- On failure the CO moves to `Failed` after retries (see [persistence](persistence.md)).

See [state-machine](state-machine.md) for where Intake sits.
