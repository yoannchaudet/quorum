<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { api, IpcError } from './lib/ipc';
  import type { RepositoryDto, WorkItemDto } from './lib/ipc';

  export let repository: RepositoryDto;
  export let onCreated: (item: WorkItemDto) => void;
  export let onClose: () => void;

  let source: 'inline' | 'file' | 'github' = 'inline';
  let title = '';
  let markdownBody = '';
  let localPath = '';
  let githubReference = '';
  let requirePlanApproval = true;
  let busy = false;
  let error = '';
  $: canCreate =
    source === 'inline'
      ? title.trim() !== '' && markdownBody.trim() !== ''
      : source === 'file'
        ? localPath.toLowerCase().endsWith('.md')
        : githubReference.trim() !== '';

  const message = (cause: unknown) =>
    cause instanceof IpcError
      ? `${cause.message}${cause.recovery ? ` ${cause.recovery}` : ''}`
      : 'Quorum could not create this work item. Please try again.';

  async function chooseMarkdown() {
    error = '';
    try {
      const selection = await open({
        directory: false,
        multiple: false,
        title: 'Choose Markdown Work Item',
        filters: [{ name: 'Markdown', extensions: ['md'] }]
      });
      if (typeof selection === 'string') localPath = selection;
    } catch (cause) {
      error = message(cause);
    }
  }

  async function create() {
    if (!canCreate) return;
    busy = true;
    error = '';
    try {
      const common = {
        repositoryId: repository.id,
        requirePlanApproval
      };
      const item =
        source === 'inline'
          ? await api.intakeInlineMarkdown({
              ...common,
              title: title.trim(),
              markdownBody
            })
          : source === 'file'
            ? await api.intakeLocalMarkdown({ ...common, path: localPath })
            : await api.intakeGithubIssue({
                ...common,
                reference: githubReference.trim()
              });
      onCreated(item);
    } catch (cause) {
      error = message(cause);
    } finally {
      busy = false;
    }
  }
</script>

<div class="scrim" role="presentation">
  <dialog open class="sheet work-sheet" aria-labelledby="new-work-title" aria-modal="true">
    <h2 id="new-work-title">New Work Item</h2>
    <p>Choose one durable source for planning.</p>

    <fieldset class="source-choices">
      <legend>Work item source</legend>
      <label>
        <input type="radio" name="source" value="inline" bind:group={source} />
        Inline Markdown
      </label>
      <label>
        <input type="radio" name="source" value="file" bind:group={source} />
        Local .md file
      </label>
      <label>
        <input type="radio" name="source" value="github" bind:group={source} />
        GitHub issue
      </label>
    </fieldset>

    {#if source === 'inline'}
      <label>
        Title
        <input bind:value={title} autocomplete="off" />
      </label>
      <label>
        Markdown
        <textarea
          bind:value={markdownBody}
          rows="10"
          placeholder="Describe the outcome, constraints, and acceptance criteria…"
        ></textarea>
      </label>
    {:else if source === 'file'}
      <div class="file-picker">
        <span class:empty={!localPath}>{localPath || 'No Markdown file selected'}</span>
        <button type="button" on:click={chooseMarkdown}>Choose .md File…</button>
      </div>
    {:else}
      <label>
        GitHub issue URL or owner/repo#number
        <input
          bind:value={githubReference}
          autocomplete="off"
          placeholder="https://github.com/owner/repo/issues/42"
        />
      </label>
    {/if}

    <label class="approval-choice">
      <input type="checkbox" bind:checked={requirePlanApproval} />
      Require plan approval before enqueue
    </label>

    {#if error}<div role="alert" class="dialog-error">{error}</div>{/if}

    <div class="dialog-actions">
      <button type="button" class="cancel" disabled={busy} on:click={onClose}>Cancel</button>
      <button type="button" class="primary" disabled={!canCreate || busy} on:click={create}>
        {busy ? 'Creating…' : 'Create Work Item'}
      </button>
    </div>
  </dialog>
</div>

<style>
  .source-choices {
    display: flex;
    gap: 8px;
    margin: 18px 0;
    padding: 0;
    border: 0;
  }

  .source-choices legend {
    width: 100%;
    margin-bottom: 7px;
    color: #55555a;
    font-size: 12px;
  }

  .source-choices label {
    display: flex;
    flex: 1;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 9px;
    border: 1px solid var(--hairline);
    border-radius: 7px;
    background: white;
  }

  .source-choices input,
  .approval-choice input {
    width: auto;
    margin: 0;
  }

  .file-picker {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 18px 0;
    padding: 10px;
    border: 1px solid var(--hairline);
    border-radius: 7px;
    background: white;
  }

  .file-picker span {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-picker span.empty {
    color: var(--secondary);
  }

  .file-picker button {
    padding: 6px 10px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: white;
    font-size: 12px;
  }

  .approval-choice {
    display: flex;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 8px;
    margin-top: 18px;
  }

  .dialog-error {
    margin: 12px 0;
    padding: 9px 11px;
    border: 1px solid rgba(215, 0, 21, 0.18);
    border-radius: 7px;
    color: #8a0712;
    background: #fff0f1;
    font-size: 12px;
  }

  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 20px;
  }
</style>
