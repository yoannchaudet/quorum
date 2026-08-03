<script lang="ts">
  import { onMount } from 'svelte';
  import { api, IpcError } from './lib/ipc';
  import type { ExecutionDetailDto, ExecutionPhaseEventDto } from './lib/ipc';

  export let workItemId: string;
  export let queueEntryId: string;

  let detail: ExecutionDetailDto | null = null;
  let loading = true;
  let busyAction = '';
  let error = '';
  let dispositions: Record<string, string> = {};

  const activeStatuses = new Set([
    'starting',
    'building',
    'verifying',
    'reviewing',
    'remediating',
    'cancelling'
  ]);

  const message = (cause: unknown) =>
    cause instanceof IpcError
      ? `${cause.message}${cause.recovery ? ` ${cause.recovery}` : ''}`
      : 'Quorum could not refresh execution. Please try again.';

  const identifier = () =>
    globalThis.crypto?.randomUUID?.() ??
    `quorum-${Date.now()}-${Math.random().toString(16).slice(2)}`;

  const label = (value: string) =>
    value.replaceAll('_', ' ').replace(/\b\w/g, (character) => character.toUpperCase());

  function eventText(event: ExecutionPhaseEventDto) {
    const payload = event.payload;
    if (typeof payload === 'string') return payload;
    if (payload && typeof payload === 'object') {
      const record = payload as Record<string, unknown>;
      for (const key of ['message', 'diagnostic', 'summary', 'reason', 'status']) {
        if (typeof record[key] === 'string') return record[key] as string;
      }
      try {
        return JSON.stringify(payload);
      } catch {
        return label(event.eventKind);
      }
    }
    return label(event.eventKind);
  }

  async function refresh(showLoading = false) {
    if (showLoading) loading = true;
    try {
      detail = await api.getExecution(workItemId);
      error = '';
    } catch (cause) {
      error = message(cause);
    } finally {
      loading = false;
    }
  }

  async function act(name: string, operation: () => Promise<ExecutionDetailDto>) {
    busyAction = name;
    error = '';
    try {
      detail = await operation();
    } catch (cause) {
      const actionError = message(cause);
      await refresh();
      error = actionError;
    } finally {
      busyAction = '';
    }
  }

  async function startExecution() {
    await act('start', () =>
      api.startExecution({
        queueEntryId,
        idempotencyKey: identifier()
      })
    );
  }

  async function resumeExecution() {
    if (!detail) return;
    await act('resume', () => api.resumeExecution({ runId: detail!.run.id }));
  }

  async function cancelExecution() {
    if (!detail) return;
    await act('cancel', () => api.cancelExecution({ runId: detail!.run.id }));
  }

  async function resolveFinding(findingId: string) {
    if (!detail || !dispositions[findingId]?.trim()) return;
    const note = dispositions[findingId].trim();
    await act(`resolve-${findingId}`, () =>
      api.resolveExecutionFinding({
        runId: detail!.run.id,
        findingId,
        dispositionNote: note
      })
    );
  }

  onMount(() => {
    void refresh(true);
    const poll = window.setInterval(() => {
      if (
        document.visibilityState === 'visible' &&
        (busyAction !== '' || (detail && activeStatuses.has(detail.run.status)))
      ) {
        void refresh();
      }
    }, 1500);
    const focus = () => void refresh();
    window.addEventListener('focus', focus);
    return () => {
      window.clearInterval(poll);
      window.removeEventListener('focus', focus);
    };
  });
</script>

<section class="execution-section" aria-labelledby="execution-heading">
  <div class="execution-heading">
    <div>
      <h2 id="execution-heading">Implementation and adversarial review</h2>
      <p>One persisted run owns one managed branch and worktree.</p>
    </div>
    {#if detail}
      <div class="execution-phase" aria-label={`Execution ${label(detail.run.status)}`}>
        <span>{label(detail.run.phase)}</span>
        <strong>{label(detail.run.status)}</strong>
      </div>
    {/if}
  </div>

  {#if error}
    <div role="alert" class="execution-error">
      <span>{error}</span>
      <button on:click={() => refresh()}>Refresh</button>
    </div>
  {/if}

  {#if loading}
    <p class="muted" role="status">Loading execution state…</p>
  {:else if !detail}
    <div class="start-execution-card">
      <div>
        <strong>Queued plan ready for execution</strong>
        <p>
          Quorum will capture the current clean HEAD, create a dedicated managed worktree,
          run the builder and repository verification, then invoke an independent reviewer.
        </p>
      </div>
      <button class="primary" disabled={busyAction !== ''} on:click={startExecution}>
        {busyAction === 'start' ? 'Starting…' : 'Start Execution'}
      </button>
    </div>
  {:else}
    {#if detail.run.errorMessage}
      <div class="execution-error" role="alert">
        <div>
          <strong>{label(detail.run.errorCode ?? 'execution blocked')}</strong>
          <p>{detail.run.errorMessage}</p>
        </div>
        {#if detail.canResume}
          <button disabled={busyAction !== ''} on:click={resumeExecution}>
            {busyAction === 'resume' ? 'Resuming…' : 'Resume'}
          </button>
        {/if}
      </div>
    {/if}

    {#if detail.deliveryReady}
      <div class="delivery-ready" role="status">
        <strong>Ready for delivery</strong>
        <span>Verification passed and no blocking findings remain.</span>
      </div>
    {/if}

    <div class="execution-actions">
      {#if detail.canResume}
        <button disabled={busyAction !== ''} on:click={resumeExecution}>
          {busyAction === 'resume' ? 'Resuming…' : 'Resume Execution'}
        </button>
      {/if}
      {#if detail.canCancel}
        <button class="danger" disabled={busyAction !== ''} on:click={cancelExecution}>
          {busyAction === 'cancel' ? 'Cancelling…' : 'Cancel Owned Processes'}
        </button>
      {/if}
    </div>

    <dl class="execution-metadata">
      <div><dt>Base commit</dt><dd><code>{detail.run.baseCommit ?? 'Unavailable'}</code></dd></div>
      <div><dt>Managed branch</dt><dd><code>{detail.run.branchName}</code></dd></div>
      <div><dt>Managed worktree</dt><dd><code>{detail.run.worktreePath}</code></dd></div>
      <div>
        <dt>Verification</dt>
        <dd>
          <code>
            {detail.run.verificationProgram ?? 'Unavailable'}
            {detail.run.verificationArguments.join(' ')}
          </code>
        </dd>
      </div>
      <div>
        <dt>Builder</dt>
        <dd>{detail.run.builderModel}<code>{detail.run.builderSessionName}</code></dd>
      </div>
      <div>
        <dt>Reviewer</dt>
        <dd>{detail.run.reviewerModel}<code>{detail.run.reviewerSessionName}</code></dd>
      </div>
      <div>
        <dt>Remediation</dt>
        <dd>{detail.run.iteration} / {detail.run.maxIterations}</dd>
      </div>
      <div>
        <dt>Owned attempts</dt>
        <dd>{detail.attempts.length}</dd>
      </div>
    </dl>

    {#if detail.findings.length > 0}
      <div class="findings" aria-labelledby="findings-heading">
        <h3 id="findings-heading">
          Review findings
          {#if detail.blockingFindingCount > 0}
            <span>{detail.blockingFindingCount} blocking</span>
          {/if}
        </h3>
        {#each detail.findings as finding (finding.id)}
          <article class:blocking={finding.severity === 'blocking'} class="finding">
            <header>
              <strong>{finding.title}</strong>
              <span>{label(finding.severity)} · {label(finding.status)}</span>
            </header>
            {#if finding.path}
              <code>{finding.path}{finding.line ? `:${finding.line}` : ''}</code>
            {/if}
            <p>{finding.body}</p>
            {#if finding.dispositionNote}
              <div class="disposition"><strong>Disposition</strong> {finding.dispositionNote}</div>
            {:else if finding.severity === 'blocking' && finding.status === 'open' && detail.run.status === 'blocked'}
              <label for={`disposition-${finding.id}`}>Disposition note</label>
              <textarea
                id={`disposition-${finding.id}`}
                rows="3"
                bind:value={dispositions[finding.id]}
                placeholder="Explain why this finding is explicitly resolved…"
              ></textarea>
              <button
                disabled={!dispositions[finding.id]?.trim() || busyAction !== ''}
                on:click={() => resolveFinding(finding.id)}
              >
                {busyAction === `resolve-${finding.id}` ? 'Resolving…' : 'Resolve Finding'}
              </button>
            {/if}
          </article>
        {/each}
      </div>
    {/if}

    {#if detail.recentLogs.length > 0}
      <details class="process-output" open>
        <summary>Bounded process output ({detail.recentLogs.length})</summary>
        <ol>
          {#each detail.recentLogs as log (log.id)}
            <li>
              <div>
                <strong>{label(log.phase)}</strong>
                <span>{log.program} · {log.stream}{log.truncated ? ' · truncated' : ''}</span>
              </div>
              <pre>{log.text}</pre>
            </li>
          {/each}
        </ol>
      </details>
    {/if}

    {#if detail.recentEvents.length > 0}
      <details class="execution-events">
        <summary>Durable phase history ({detail.recentEvents.length})</summary>
        <ol>
          {#each [...detail.recentEvents].reverse() as event (event.id)}
            <li>
              <strong>{label(event.eventKind)}</strong>
              <span>{eventText(event)}</span>
            </li>
          {/each}
        </ol>
      </details>
    {/if}
  {/if}
</section>

<style>
  .execution-section {
    padding-top: 26px;
    border-top: 1px solid var(--hairline);
  }

  .execution-heading,
  .start-execution-card,
  .execution-error,
  .delivery-ready,
  .execution-actions,
  .finding header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
  }

  .execution-heading h2,
  .execution-heading p {
    margin: 0;
  }

  .execution-heading h2 {
    font-size: 17px;
  }

  .execution-heading p,
  .start-execution-card p,
  .execution-phase span,
  .finding header span,
  .process-output li div span {
    color: var(--secondary);
    font-size: 12px;
  }

  .execution-phase {
    min-width: 130px;
    padding: 8px 11px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: white;
    text-align: right;
  }

  .execution-phase span,
  .execution-phase strong {
    display: block;
  }

  .start-execution-card,
  .execution-error,
  .delivery-ready {
    margin-top: 16px;
    padding: 14px;
    border: 1px solid var(--hairline);
    border-radius: 9px;
    background: white;
  }

  .start-execution-card p,
  .execution-error p {
    margin: 4px 0 0;
  }

  .execution-error {
    border-color: rgba(215, 0, 21, 0.2);
    color: #8a0712;
    background: #fff0f1;
  }

  .delivery-ready {
    justify-content: flex-start;
    color: #287a38;
    background: #e7f7e9;
  }

  .execution-actions {
    justify-content: flex-end;
    margin-top: 12px;
  }

  button {
    padding: 6px 10px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: white;
    font-size: 12px;
  }

  button.primary {
    border-color: transparent;
    color: white;
    background: var(--accent);
  }

  button.danger {
    color: #b00020;
  }

  button:disabled {
    opacity: 0.45;
  }

  .execution-metadata {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 1px;
    margin: 18px 0;
    overflow: hidden;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: var(--hairline);
  }

  .execution-metadata div {
    min-width: 0;
    padding: 10px;
    background: white;
  }

  .execution-metadata dt {
    color: var(--secondary);
    font-size: 10px;
    text-transform: uppercase;
  }

  .execution-metadata dd {
    margin: 4px 0 0;
    overflow-wrap: anywhere;
    font-size: 12px;
  }

  .execution-metadata code {
    display: block;
    font-size: 10px;
  }

  .findings h3 {
    font-size: 14px;
  }

  .findings h3 span {
    margin-left: 8px;
    color: #8a0712;
    font-size: 11px;
  }

  .finding {
    margin-top: 8px;
    padding: 13px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: white;
  }

  .finding.blocking {
    border-color: rgba(215, 0, 21, 0.25);
  }

  .finding p {
    margin: 8px 0;
    font-size: 13px;
  }

  .finding code {
    font-size: 10px;
  }

  .finding label {
    display: block;
    margin-top: 10px;
    color: var(--secondary);
    font-size: 11px;
  }

  .finding textarea {
    width: 100%;
    margin: 5px 0 8px;
    padding: 8px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    resize: vertical;
  }

  .disposition {
    padding: 8px;
    background: #f3f3f5;
    font-size: 12px;
  }

  .process-output,
  .execution-events {
    margin-top: 16px;
  }

  .process-output summary,
  .execution-events summary {
    color: var(--secondary);
    font-size: 12px;
  }

  .process-output ol,
  .execution-events ol {
    margin: 8px 0 0;
    padding: 0;
    list-style: none;
  }

  .process-output li,
  .execution-events li {
    padding: 8px 0;
    border-top: 1px solid var(--hairline);
  }

  .process-output li div {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 11px;
  }

  .process-output pre {
    margin: 5px 0 0;
    max-height: 180px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 10px;
  }

  .execution-events li {
    display: grid;
    grid-template-columns: 180px 1fr;
    gap: 10px;
    font-size: 11px;
  }
</style>
