<script lang="ts">
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { onMount } from 'svelte';
  import { api, IpcError } from './lib/ipc';
  import { renderMarkdown } from './lib/markdown';
  import type {
    PlanningAgentDto,
    PlanningDetailDto,
    PlanningEventDto,
    WorkItemDto
  } from './lib/ipc';

  export let workItem: WorkItemDto;

  let detail: PlanningDetailDto | null = null;
  let loading = true;
  let busyAction = '';
  let error = '';
  let noPlanning = false;
  let answers: Record<string, string> = {};
  let planDraft = '';
  let planMode: 'preview' | 'edit' = 'preview';
  let planDirty = false;

  const activeStatuses = new Set([
    'pending',
    'running',
    'waiting_for_answers',
    'synthesizing',
    'blocked'
  ]);

  const message = (cause: unknown) =>
    cause instanceof IpcError
      ? `${cause.message}${cause.recovery ? ` ${cause.recovery}` : ''}`
      : 'Quorum could not refresh planning. Please try again.';

  const linkMessage = (cause: unknown) =>
    `Quorum could not open that link.${cause instanceof Error && cause.message ? ` ${cause.message}` : ''}`;

  const identifier = () =>
    globalThis.crypto?.randomUUID?.() ??
    `quorum-${Date.now()}-${Math.random().toString(16).slice(2)}`;

  function syncDetail(next: PlanningDetailDto, forcePlan = false) {
    detail = next;
    noPlanning = false;
    if (next.plan && (forcePlan || !planDirty)) {
      planDraft = next.plan.markdownBody;
      planDirty = false;
    }
  }

  async function refresh(showLoading = false) {
    if (showLoading) loading = true;
    try {
      syncDetail(await api.getPlanning(workItem.id));
      error = '';
    } catch (cause) {
      if (
        cause instanceof IpcError &&
        (cause.code === 'not_found' || cause.message.includes('has not been started'))
      ) {
        if (busyAction !== 'start') {
          noPlanning = true;
          detail = null;
          error = '';
        }
      } else {
        error = message(cause);
      }
    } finally {
      loading = false;
    }
  }

  async function act(
    name: string,
    operation: () => Promise<PlanningDetailDto>,
    forcePlan = false
  ): Promise<boolean> {
    busyAction = name;
    error = '';
    try {
      syncDetail(await operation(), forcePlan);
      return true;
    } catch (cause) {
      const actionError = message(cause);
      await refresh();
      error = actionError;
      return false;
    } finally {
      busyAction = '';
    }
  }

  async function startPlanning() {
    await act('start', () =>
      api.startPlanning({ workItemId: workItem.id, idempotencyKey: identifier() })
    );
  }

  function pendingQuestionsFor(planningAgentId: string) {
    return detail?.pendingQuestions.filter(
      (question) => question.planningAgentId === planningAgentId
    ) ?? [];
  }

  function pendingAgentIds() {
    return [...new Set(detail?.pendingQuestions.map((question) => question.planningAgentId) ?? [])];
  }

  function answersReady(planningAgentId: string) {
    const questions = pendingQuestionsFor(planningAgentId);
    return questions.length > 0 && questions.every((question) => answers[question.id]?.trim());
  }

  async function submitAnswers(planningAgentId: string) {
    if (!detail) return;
    const questions = pendingQuestionsFor(planningAgentId);
    if (!answersReady(planningAgentId)) return;
    const submitted = await act(`answer-${planningAgentId}`, () =>
      api.submitPlanningAnswers({
        planningRunId: detail!.run.id,
        planningAgentId,
        expectedRunUpdatedAt: detail!.run.updatedAt,
        answers: questions.map((question) => ({
          questionId: question.id,
          answerMarkdown: answers[question.id].trim()
        }))
      })
    );
    if (submitted) {
      answers = {
        ...answers,
        ...Object.fromEntries(questions.map((question) => [question.id, '']))
      };
    }
  }

  async function openTerminal(planningAgentId: string) {
    await act(`terminal-${planningAgentId}`, () =>
      api.openPlanningTerminal({
        workItemId: workItem.id,
        planningAgentId,
        idempotencyKey: identifier()
      })
    );
  }

  async function reconcileTerminal() {
    if (!detail?.terminalHandoff) return;
    await act('reconcile', () =>
      api.reconcilePlanningTerminal({
        workItemId: workItem.id,
        terminalHandoffId: detail!.terminalHandoff!.id
      })
    );
  }

  async function retryAgent(agentId: string | null) {
    if (!detail) return;
    await act(`retry-${agentId ?? 'run'}`, () =>
      api.retryPlanningAgent({
        planningRunId: detail!.run.id,
        planningAgentId: agentId,
        expectedRunUpdatedAt: detail!.run.updatedAt
      })
    );
  }

  async function savePlan() {
    if (!detail?.plan || !planDraft.trim()) return;
    const saved = await act(
      'save-plan',
      () =>
        api.updateSynthesizedPlan({
          planningRunId: detail!.run.id,
          expectedPlanUpdatedAt: detail!.plan!.updatedAt,
          markdownBody: planDraft
        }),
      true
    );
    if (saved) planMode = 'preview';
  }

  async function decidePlan(decision: 'approve' | 'reject') {
    if (!detail?.plan) return;
    await act(decision, () =>
      (decision === 'approve' ? api.approvePlan : api.rejectPlan)({
        planningRunId: detail!.run.id,
        expectedPlanUpdatedAt: detail!.plan!.updatedAt
      })
    );
  }

  async function enqueue() {
    if (!detail) return;
    await act('enqueue', () => api.enqueuePlan({ planningRunId: detail!.run.id }));
  }

  function agentFor(id: string) {
    return detail?.agents.find((agent) => agent.id === id);
  }

  function agentName(agent: PlanningAgentDto | undefined) {
    if (!agent) return 'Planning agent';
    return agent.role === 'synthesizer'
      ? 'Synthesizer'
      : `Planner ${agent.ordinal + 1}`;
  }

  function label(value: string) {
    return value.replaceAll('_', ' ').replace(/\b\w/g, (character) => character.toUpperCase());
  }

  function eventText(event: PlanningEventDto) {
    const payload = event.payload;
    if (typeof payload === 'string') return payload;
    if (payload && typeof payload === 'object') {
      const record = payload as Record<string, unknown>;
      for (const key of ['message', 'text', 'content', 'delta', 'status']) {
        if (typeof record[key] === 'string') return record[key] as string;
      }
      try {
        return JSON.stringify(payload);
      } catch {
        return 'Event details are unavailable.';
      }
    }
    return String(payload ?? event.eventKind ?? 'Planning event');
  }

  function externalMarkdownLinks(node: HTMLElement) {
    const handleClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const link = target.closest<HTMLAnchorElement>('a[href]');
      if (!link) return;
      event.preventDefault();
      void openUrl(link.href).catch((cause) => (error = linkMessage(cause)));
    };
    node.addEventListener('click', handleClick);
    return { destroy: () => node.removeEventListener('click', handleClick) };
  }

  onMount(() => {
    void refresh(true);
    const focus = () => {
      if (!noPlanning) void refresh();
    };
    const poll = window.setInterval(() => {
      if (
        document.visibilityState === 'visible' &&
        (busyAction === 'start' ||
          (detail &&
            (activeStatuses.has(detail.run.status) ||
          detail.terminalHandoff?.status === 'awaiting_manual_reconcile')
          ))
      ) {
        void refresh();
      }
    }, 2000);
    window.addEventListener('focus', focus);
    return () => {
      window.clearInterval(poll);
      window.removeEventListener('focus', focus);
    };
  });
</script>

<div class="planning-page">
  {#if error}
    <div role="alert" class="planning-error">
      <span>{error}</span>
      <button on:click={() => refresh()}>Refresh</button>
    </div>
  {/if}

  {#if loading}
    <div class="planning-empty" role="status">Loading planning state…</div>
  {:else if noPlanning}
    <section class="work-source" aria-labelledby="work-title">
      <div class="eyebrow">{label(workItem.sourceKind)}</div>
      <h1 id="work-title">{workItem.title}</h1>
      <div class="markdown" use:externalMarkdownLinks>
        {@html renderMarkdown(workItem.markdownBody)}
      </div>
      <div class="start-card">
        <h2>Ready to plan</h2>
        <p>
          Quorum will run planners and a synthesizer, then stop for review. It will not begin
          implementation.
        </p>
        <p>
          Plan approval:
          <strong>{workItem.requirePlanApproval ? 'Required' : 'Optional'}</strong>
        </p>
        <button class="primary" disabled={busyAction !== ''} on:click={startPlanning}>
          {busyAction === 'start' ? 'Starting planning…' : 'Start Planning'}
        </button>
      </div>
    </section>
  {:else if detail}
    <header class="planning-header">
      <div>
        <div class="eyebrow">{detail.source.kind.replaceAll('_', ' ')}</div>
        <h1>{detail.source.title}</h1>
      </div>
      <div class="phase" aria-label={`Current phase ${label(detail.currentPhase)}`}>
        <span>{label(detail.currentPhase)}</span>
        <strong>{label(detail.status)}</strong>
      </div>
    </header>

    {#if detail.run.errorMessage}
      <div role="alert" class="failure-card">
        <div>
          <strong>Planning {label(detail.run.status)}</strong>
          <p>{detail.run.errorMessage}</p>
          {#if detail.run.errorCode}<code>{detail.run.errorCode}</code>{/if}
        </div>
        <button disabled={busyAction !== ''} on:click={() => retryAgent(null)}>
          Retry planning
        </button>
      </div>
    {/if}

    <section aria-labelledby="agents-heading">
      <h2 id="agents-heading">Planning team</h2>
      <div class="agent-grid">
        {#each detail.agents as agent (agent.id)}
          <article class:failed={agent.status === 'failed'} class="agent-card">
            <div class="agent-heading">
              <strong>{agentName(agent)}</strong>
              <span class={`status-pill ${agent.status}`}>{label(agent.status)}</span>
            </div>
            <p>{agent.modelId}</p>
            <code title="Named Copilot session">{agent.sessionName}</code>
            {#if agent.errorMessage}
              <div class="agent-error">
                <span>{agent.errorMessage}</span>
                <button
                  disabled={busyAction !== ''}
                  on:click={() => retryAgent(agent.id)}>Retry agent</button
                >
              </div>
            {/if}
          </article>
        {/each}
      </div>
    </section>

    {#if detail.pendingQuestions.length > 0 || detail.answeredQuestions.length > 0}
      <section aria-labelledby="questions-heading">
        <h2 id="questions-heading">Questions</h2>
        {#each pendingAgentIds() as planningAgentId (planningAgentId)}
          <article class="question-card pending">
            <div class="question-meta">
              <strong>{agentName(agentFor(planningAgentId))}</strong>
              <span>Needs your answer</span>
            </div>
            {#each pendingQuestionsFor(planningAgentId) as question (question.id)}
              <div class="question-prompt">
                <div class="markdown" use:externalMarkdownLinks>
                  {@html renderMarkdown(question.promptMarkdown)}
                </div>
                <label for={`answer-${question.id}`}>Answer</label>
                <textarea
                  id={`answer-${question.id}`}
                  rows="4"
                  bind:value={answers[question.id]}
                  placeholder="Answer in Markdown…"
                ></textarea>
              </div>
            {/each}
            <div class="question-actions">
              <button
                disabled={busyAction !== ''}
                on:click={() => openTerminal(planningAgentId)}
              >
                {busyAction === `terminal-${planningAgentId}`
                  ? 'Opening…'
                  : `Open ${agentFor(planningAgentId)?.sessionName ?? agentName(agentFor(planningAgentId))} in Terminal`}
              </button>
              <button
                class="primary"
                disabled={!answersReady(planningAgentId) || busyAction !== ''}
                on:click={() => submitAnswers(planningAgentId)}
              >
                {busyAction === `answer-${planningAgentId}`
                  ? 'Submitting…'
                  : pendingQuestionsFor(planningAgentId).length === 1
                    ? 'Submit Answer'
                    : `Submit ${pendingQuestionsFor(planningAgentId).length} Answers`}
              </button>
            </div>
          </article>
        {/each}

        {#if detail.answeredQuestions.length > 0}
          <details class="answered" open={detail.pendingQuestions.length === 0}>
            <summary>Answered questions ({detail.answeredQuestions.length})</summary>
            {#each detail.answeredQuestions as question (question.id)}
              <article class="question-card">
                <strong>{agentName(agentFor(question.planningAgentId))}</strong>
                <div class="markdown" use:externalMarkdownLinks>
                  {@html renderMarkdown(question.promptMarkdown)}
                </div>
                <div class="answer">
                  <span>Your answer</span>
                  <div class="markdown" use:externalMarkdownLinks>
                    {@html renderMarkdown(question.answerMarkdown ?? '')}
                  </div>
                </div>
              </article>
            {/each}
          </details>
        {/if}
      </section>
    {/if}

    {#if detail.terminalHandoff}
      <section class="terminal-card" aria-labelledby="terminal-state-heading">
        <div>
          <h2 id="terminal-state-heading">Terminal handoff</h2>
          <p>
            {agentName(agentFor(detail.terminalHandoff.planningAgentId))} ·
            {label(detail.terminalHandoff.status)}
          </p>
          <code title="Exact Copilot session">{detail.terminalHandoff.sessionName}</code>
          {#if detail.terminalHandoff.errorMessage}
            <div role="alert">{detail.terminalHandoff.errorMessage}</div>
          {/if}
        </div>
        {#if detail.terminalHandoff.manualReconcileAvailable}
          <button
            aria-label={`Resume and reconcile ${detail.terminalHandoff.sessionName} manually`}
            disabled={busyAction !== ''}
            on:click={reconcileTerminal}
          >
            {busyAction === 'reconcile' ? 'Reconciling…' : 'Resume / Reconcile Manually'}
          </button>
        {/if}
      </section>
    {/if}

    {#if detail.recentEvents.length > 0}
      <section aria-labelledby="events-heading">
        <h2 id="events-heading">Recent activity</h2>
        <ol class="events">
          {#each [...detail.recentEvents].reverse() as event (event.id)}
            <li>
              <div>
                <strong>{agentName(agentFor(event.planningAgentId))}</strong>
                <span>{event.eventKind ? label(event.eventKind) : `Event ${event.sequence}`}</span>
              </div>
              <p>{eventText(event)}</p>
            </li>
          {/each}
        </ol>
      </section>
    {/if}

    {#if detail.plan}
      <section class="plan-section" aria-labelledby="plan-heading">
        <div class="plan-toolbar">
          <div>
            <h2 id="plan-heading">Synthesized plan</h2>
            <p>
              Revision {detail.plan.revision}.{detail.plan.editRevision} ·
              {detail.plan.approvalPolicy === 'required'
                ? `Approval ${label(detail.plan.approvalStatus)}`
                : 'Approval not required'}
            </p>
          </div>
          <div role="group" aria-label="Plan view">
            <button
              class:active={planMode === 'preview'}
              aria-pressed={planMode === 'preview'}
              on:click={() => (planMode = 'preview')}>Preview</button
            >
            <button
              class:active={planMode === 'edit'}
              aria-pressed={planMode === 'edit'}
              on:click={() => (planMode = 'edit')}>Edit</button
            >
          </div>
        </div>

        {#if planMode === 'edit'}
          <label class="sr-only" for="plan-editor">Synthesized plan Markdown</label>
          <textarea
            id="plan-editor"
            class="plan-editor"
            rows="20"
            bind:value={planDraft}
            on:input={() => (planDirty = planDraft !== detail?.plan?.markdownBody)}
          ></textarea>
        {:else}
          <div class="plan-preview markdown" use:externalMarkdownLinks>
            {@html renderMarkdown(planDraft)}
          </div>
        {/if}

        <div class="plan-actions">
          {#if planDirty}
            <button
              disabled={!planDraft.trim() || busyAction !== ''}
              on:click={() => {
                planDraft = detail?.plan?.markdownBody ?? '';
                planDirty = false;
              }}>Discard edits</button
            >
            <button
              class="primary"
              disabled={!planDraft.trim() || busyAction !== ''}
              on:click={savePlan}
            >
              {busyAction === 'save-plan' ? 'Saving…' : 'Save Revision'}
            </button>
          {/if}

          {#if detail.plan.approvalPolicy === 'required'}
            <button
              disabled={planDirty || busyAction !== ''}
              on:click={() => decidePlan('reject')}>Reject</button
            >
            <button
              class="primary"
              disabled={planDirty || busyAction !== '' || detail.plan.approvalStatus === 'approved'}
              on:click={() => decidePlan('approve')}
            >
              {busyAction === 'approve' ? 'Approving…' : 'Approve Plan'}
            </button>
          {/if}
        </div>

        <div class="enqueue-card">
          <div>
            <strong>
              {detail.queue.entry ? `Queued at position ${detail.queue.entry.position + 1}` : 'Later implementation'}
            </strong>
            <p>
              {detail.queue.entry
                ? 'Intent is persisted. Planning has stopped; implementation has not started.'
                : detail.queue.reason ??
                  'Enqueue persists future implementation intent without starting implementation.'}
            </p>
          </div>
          <button
            class="primary"
            disabled={!detail.queue.eligible || planDirty || busyAction !== '' || !!detail.queue.entry}
            on:click={enqueue}
          >
            {busyAction === 'enqueue' ? 'Enqueueing…' : 'Enqueue'}
          </button>
        </div>
      </section>
    {/if}
  {/if}
</div>

<style>
  .planning-page {
    width: min(960px, calc(100% - 48px));
    margin: 0 auto;
    padding: 34px 0 72px;
  }

  .planning-empty {
    padding: 70px 0;
    color: var(--secondary);
    text-align: center;
  }

  .planning-error,
  .failure-card,
  .terminal-card,
  .enqueue-card {
    display: flex;
    align-items: center;
    gap: 18px;
    justify-content: space-between;
    margin-bottom: 22px;
    padding: 13px 15px;
    border: 1px solid var(--hairline);
    border-radius: 9px;
    background: white;
  }

  .planning-error,
  .failure-card {
    border-color: rgba(215, 0, 21, 0.18);
    color: #8a0712;
    background: #fff0f1;
  }

  .planning-error button,
  .failure-card button,
  .terminal-card button,
  .plan-actions button,
  .plan-toolbar button,
  .question-actions button,
  .agent-error button,
  .enqueue-card button {
    padding: 6px 10px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: white;
    font-size: 12px;
  }

  .planning-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 24px;
    margin-bottom: 32px;
  }

  .planning-header h1,
  .work-source h1 {
    margin: 3px 0 0;
    font-size: 30px;
    letter-spacing: -0.035em;
  }

  .eyebrow {
    color: var(--secondary);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
  }

  .phase {
    min-width: 130px;
    padding: 9px 12px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: white;
    text-align: right;
  }

  .phase span,
  .phase strong {
    display: block;
  }

  .phase span {
    color: var(--secondary);
    font-size: 11px;
  }

  .phase strong {
    margin-top: 2px;
    font-size: 13px;
  }

  section {
    margin-top: 32px;
  }

  section > h2,
  .terminal-card h2,
  .plan-toolbar h2,
  .start-card h2 {
    margin: 0 0 10px;
    font-size: 17px;
  }

  .agent-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 10px;
  }

  .agent-card {
    padding: 13px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: white;
  }

  .agent-card.failed {
    border-color: rgba(215, 0, 21, 0.25);
  }

  .agent-heading,
  .question-meta,
  .plan-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .agent-card p,
  .agent-card code {
    display: block;
    margin: 7px 0 0;
    color: var(--secondary);
    font-size: 11px;
  }

  .agent-card code {
    overflow-wrap: anywhere;
  }

  .status-pill {
    padding: 2px 7px;
    border-radius: 999px;
    color: #4d4d51;
    background: #eeeef0;
    font-size: 10px;
  }

  .status-pill.running,
  .status-pill.synthesizing {
    color: #0057ad;
    background: #e4f1ff;
  }

  .status-pill.succeeded {
    color: #287a38;
    background: #e7f7e9;
  }

  .status-pill.failed {
    color: #8a0712;
    background: #fff0f1;
  }

  .agent-error {
    display: grid;
    gap: 8px;
    margin-top: 10px;
    color: #8a0712;
    font-size: 12px;
  }

  .question-card {
    margin: 10px 0;
    padding: 16px;
    border: 1px solid var(--hairline);
    border-radius: 9px;
    background: white;
  }

  .question-card.pending {
    border-color: rgba(0, 122, 255, 0.3);
  }

  .question-meta span,
  .answer > span,
  .plan-toolbar p,
  .terminal-card p,
  .enqueue-card p {
    color: var(--secondary);
    font-size: 12px;
  }

  .question-card label {
    display: block;
    margin: 12px 0 5px;
    color: var(--secondary);
    font-size: 12px;
  }

  .question-card textarea,
  .plan-editor {
    width: 100%;
    padding: 10px;
    border: 1px solid var(--hairline);
    border-radius: 7px;
    background: #fff;
    font-family: "SFMono-Regular", ui-monospace, monospace;
    font-size: 13px;
    line-height: 1.5;
    resize: vertical;
  }

  .question-actions,
  .plan-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 10px;
  }

  .answered {
    margin-top: 14px;
  }

  .answered summary {
    color: var(--secondary);
    font-size: 12px;
  }

  .answer {
    margin-top: 12px;
    padding: 10px;
    border-left: 3px solid #c6c6ca;
    background: #f7f7f8;
  }

  .terminal-card {
    border-color: rgba(180, 110, 0, 0.22);
    background: #fff7e5;
  }

  .terminal-card h2,
  .terminal-card p,
  .enqueue-card p {
    margin: 0;
  }

  .events {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .events li {
    display: grid;
    grid-template-columns: 170px 1fr;
    gap: 14px;
    padding: 9px 0;
    border-top: 1px solid var(--hairline);
  }

  .events li div,
  .events li strong,
  .events li span {
    display: block;
  }

  .events li span {
    color: var(--secondary);
    font-size: 10px;
  }

  .events li p {
    margin: 0;
    overflow-wrap: anywhere;
    font-size: 12px;
  }

  .plan-section {
    padding-top: 26px;
    border-top: 1px solid var(--hairline);
  }

  .plan-toolbar {
    margin-bottom: 14px;
  }

  .plan-toolbar h2,
  .plan-toolbar p {
    margin: 0;
  }

  .plan-toolbar button.active {
    color: white;
    background: #55555a;
  }

  .plan-preview {
    min-height: 180px;
    padding: 20px;
    border: 1px solid var(--hairline);
    border-radius: 9px;
    background: white;
  }

  .enqueue-card {
    margin-top: 20px;
    margin-bottom: 0;
    background: #f1f6fb;
  }

  .enqueue-card strong,
  .enqueue-card p {
    display: block;
  }

  .start-card {
    margin-top: 32px;
    padding: 18px;
    border: 1px solid var(--hairline);
    border-radius: 9px;
    background: white;
  }

  .start-card p {
    color: var(--secondary);
    font-size: 13px;
    line-height: 1.5;
  }

  .work-source > .markdown {
    margin-top: 24px;
  }

  .failure-card p {
    margin: 4px 0;
  }

  .failure-card code {
    font-size: 11px;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  button:disabled {
    opacity: 0.45;
  }
</style>
