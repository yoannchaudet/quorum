import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import PlanningView from './PlanningView.svelte';
import { IpcError, type PlanningDetailDto } from './lib/ipc';

const mocks = vi.hoisted(() => ({
  getPlanning: vi.fn(),
  startPlanning: vi.fn(),
  submitPlanningAnswers: vi.fn(),
  retryPlanningAgent: vi.fn(),
  openPlanningTerminal: vi.fn(),
  reconcilePlanningTerminal: vi.fn(),
  updateSynthesizedPlan: vi.fn(),
  approvePlan: vi.fn(),
  rejectPlan: vi.fn(),
  enqueuePlan: vi.fn()
}));

vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));
vi.mock('./lib/ipc', () => ({
  IpcError: class IpcError extends Error {
    constructor(
      readonly code: string,
      message: string,
      readonly recovery?: string | null
    ) {
      super(message);
    }
  },
  api: mocks
}));

const workItem = {
  id: 'work',
  repositoryId: 'repository',
  title: 'Build planning UX',
  sourceKind: 'inline_markdown',
  markdownBody: '# Requirements',
  lifecycleStatus: 'open',
  requirePlanApproval: true,
  createdAt: 'created',
  updatedAt: 'updated'
};

const planner = {
  id: 'planner',
  role: 'planner',
  ordinal: 0,
  modelId: 'gpt-5.6-sol',
  sessionName: 'quorum-work-planner-1',
  status: 'succeeded',
  attempt: 1,
  errorCode: null,
  errorMessage: null,
  createdAt: 'created',
  updatedAt: 'updated',
  completedAt: 'completed'
};

const synthesizer = {
  ...planner,
  id: 'synthesizer',
  role: 'synthesizer',
  modelId: 'claude-opus-5',
  sessionName: 'quorum-work-synthesizer'
};

function detail(overrides: Partial<PlanningDetailDto> = {}): PlanningDetailDto {
  return {
    source: {
      workItemId: 'work',
      repositoryId: 'repository',
      title: 'Build planning UX',
      kind: 'inline_markdown',
      reference: null,
      markdownBody: '# Requirements'
    },
    currentPhase: 'planning',
    status: 'running',
    run: {
      id: 'run',
      workItemId: 'work',
      status: 'running',
      errorCode: null,
      errorMessage: null,
      idempotencyKey: 'key',
      createdAt: 'created',
      updatedAt: 'run-version',
      completedAt: null
    },
    agents: [planner, { ...synthesizer, status: 'running', completedAt: null }],
    pendingQuestions: [],
    answeredQuestions: [],
    plan: null,
    queue: { state: 'not_ready', eligible: false, reason: 'Planning must finish.', entry: null },
    recentEvents: [
      {
        id: 'event',
        planningAgentId: 'synthesizer',
        attempt: 1,
        sequence: 2,
        eventKind: 'assistant_message',
        payload: { message: 'Combining planner recommendations' },
        createdAt: 'now'
      }
    ],
    terminalHandoff: null,
    ...overrides
  };
}

function planDetail(required = true): PlanningDetailDto {
  return detail({
    currentPhase: required ? 'approval' : 'ready',
    status: required ? 'pending' : 'eligible',
    run: {
      ...detail().run,
      status: 'succeeded',
      updatedAt: 'run-complete',
      completedAt: 'completed'
    },
    agents: [planner, synthesizer],
    plan: {
      id: 'plan',
      revision: 1,
      editRevision: 1,
      markdownBody: '# Plan\n\n1. Build the UX.',
      approvalPolicy: required ? 'required' : 'not_required',
      approvalStatus: required ? 'pending' : 'draft',
      createdAt: 'created',
      updatedAt: 'plan-version'
    },
    queue: required
      ? {
          state: 'awaiting_approval',
          eligible: false,
          reason: 'Approve the synthesized plan before enqueueing it.',
          entry: null
        }
      : { state: 'eligible', eligible: true, reason: null, entry: null }
  });
}

describe('planning work item UX', () => {
  beforeEach(() => {
    for (const mock of Object.values(mocks)) mock.mockReset();
  });

  it('starts planning and clearly stops before implementation', async () => {
    mocks.getPlanning.mockRejectedValue(
      new IpcError('not_found', 'Planning has not been started for this work item.')
    );
    mocks.startPlanning.mockResolvedValue(detail());
    render(PlanningView, { workItem });

    expect(await screen.findByText('Ready to plan')).toBeInTheDocument();
    expect(screen.getByText('Required')).toBeInTheDocument();
    expect(screen.getByText(/It will not begin implementation/)).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Start Planning' }));
    expect(mocks.startPlanning).toHaveBeenCalledWith(
      expect.objectContaining({ workItemId: 'work', idempotencyKey: expect.any(String) })
    );
  });

  it('shows durable phase, agent identities, sessions, activity, and actionable failures', async () => {
    mocks.getPlanning.mockResolvedValue(
      detail({
        status: 'failed',
        run: {
          ...detail().run,
          status: 'failed',
          errorCode: 'copilot_auth',
          errorMessage: 'Copilot authentication expired.'
        },
        agents: [
          {
            ...planner,
            status: 'failed',
            errorCode: 'copilot_auth',
            errorMessage: 'Sign in to Copilot.',
            completedAt: null
          },
          { ...synthesizer, status: 'pending', completedAt: null }
        ]
      })
    );
    mocks.retryPlanningAgent.mockResolvedValue(detail());
    render(PlanningView, { workItem });

    expect(await screen.findByLabelText('Current phase Planning')).toHaveTextContent('Failed');
    expect(screen.getByText('Planner 1')).toBeInTheDocument();
    expect(screen.getByText('quorum-work-planner-1')).toBeInTheDocument();
    expect(screen.getByText('Combining planner recommendations')).toBeInTheDocument();
    expect(screen.getByText('Copilot authentication expired.')).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Retry agent' }));
    await waitFor(() =>
      expect(mocks.retryPlanningAgent).toHaveBeenCalledWith({
        planningRunId: 'run',
        planningAgentId: 'planner',
        expectedRunUpdatedAt: 'run-version'
      })
    );
  });

  it('surfaces pending and answered questions with in-app answer and exact-agent terminal handoff', async () => {
    const questioning = detail({
      status: 'waiting_for_answers',
      run: { ...detail().run, status: 'waiting_for_answers' },
      pendingQuestions: [
        {
          id: 'question',
          planningAgentId: 'planner',
          externalId: 'q1',
          ordinal: 0,
          promptMarkdown: 'Which **platform**?',
          status: 'open',
          answerMarkdown: null,
          createdAt: 'created',
          updatedAt: 'updated'
        }
      ],
      answeredQuestions: [
        {
          id: 'answered',
          planningAgentId: 'planner',
          externalId: 'q0',
          ordinal: 0,
          promptMarkdown: 'Which color?',
          status: 'answered',
          answerMarkdown: 'System accent.',
          createdAt: 'created',
          updatedAt: 'updated'
        }
      ],
      terminalHandoff: {
        id: 'handoff',
        planningAgentId: 'planner',
        sessionName: 'quorum-work-planner-1',
        status: 'awaiting_manual_reconcile',
        manualReconcileAvailable: true,
        errorCode: null,
        errorMessage: null,
        createdAt: 'created',
        updatedAt: 'updated'
      }
    });
    mocks.getPlanning.mockResolvedValue(questioning);
    mocks.submitPlanningAnswers.mockResolvedValue(detail());
    mocks.openPlanningTerminal.mockResolvedValue(questioning);
    mocks.reconcilePlanningTerminal.mockResolvedValue(detail());
    render(PlanningView, { workItem });

    expect(await screen.findByText('Which color?')).toBeInTheDocument();
    expect(screen.getByText('System accent.')).toBeInTheDocument();
    await fireEvent.input(screen.getByLabelText('Answer'), { target: { value: 'macOS' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Submit Answer' }));
    await waitFor(() =>
      expect(mocks.submitPlanningAnswers).toHaveBeenCalledWith({
        planningRunId: 'run',
        planningAgentId: 'planner',
        expectedRunUpdatedAt: 'run-version',
        answers: [{ questionId: 'question', answerMarkdown: 'macOS' }]
      })
    );

    mocks.getPlanning.mockResolvedValue(questioning);
    render(PlanningView, { workItem });
    await screen.findByText('Needs your answer');
    await fireEvent.click(
      screen
        .getAllByRole('button', { name: 'Open quorum-work-planner-1 in Terminal' })
        .at(-1)!
    );
    expect(mocks.openPlanningTerminal).toHaveBeenCalledWith(
      expect.objectContaining({ workItemId: 'work', planningAgentId: 'planner' })
    );
    await fireEvent.click(
      screen
        .getAllByRole('button', {
          name: 'Resume and reconcile quorum-work-planner-1 manually'
        })
        .at(-1)!
    );
    expect(mocks.reconcilePlanningTerminal).toHaveBeenCalledWith({
      workItemId: 'work',
      terminalHandoffId: 'handoff'
    });
  });

  it('submits every open question for one agent and preserves answers after failure', async () => {
      const questioning = detail({
        status: 'waiting_for_answers',
        run: { ...detail().run, status: 'waiting_for_answers' },
        pendingQuestions: [
          {
            id: 'question-1',
            planningAgentId: 'planner',
            externalId: 'q1',
            ordinal: 0,
            promptMarkdown: 'Which platform?',
            status: 'open',
            answerMarkdown: null,
            createdAt: 'created',
            updatedAt: 'updated'
          },
          {
            id: 'question-2',
            planningAgentId: 'planner',
            externalId: 'q2',
            ordinal: 1,
            promptMarkdown: 'Which release?',
            status: 'open',
            answerMarkdown: null,
            createdAt: 'created',
            updatedAt: 'updated'
          }
        ]
      });
      mocks.getPlanning.mockResolvedValue(questioning);
      mocks.submitPlanningAnswers.mockRejectedValue(
        new IpcError('conflict', 'The planning state changed.', 'Review the refreshed questions.')
      );
      render(PlanningView, { workItem });

      const answerFields = await screen.findAllByLabelText('Answer');
      await fireEvent.input(answerFields[0], { target: { value: 'macOS' } });
      await fireEvent.input(answerFields[1], { target: { value: 'M2' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Submit 2 Answers' }));

      await waitFor(() =>
        expect(mocks.submitPlanningAnswers).toHaveBeenCalledWith({
          planningRunId: 'run',
          planningAgentId: 'planner',
          expectedRunUpdatedAt: 'run-version',
          answers: [
            { questionId: 'question-1', answerMarkdown: 'macOS' },
            { questionId: 'question-2', answerMarkdown: 'M2' }
          ]
        })
      );
      expect(answerFields[0]).toHaveValue('macOS');
      expect(answerFields[1]).toHaveValue('M2');
      expect(screen.getByRole('alert')).toHaveTextContent(
        'The planning state changed. Review the refreshed questions.'
      );
  });

  it('keeps a failed plan revision editable and reports the actionable error', async () => {
      mocks.getPlanning.mockResolvedValue(planDetail(true));
      mocks.updateSynthesizedPlan.mockRejectedValue(
        new IpcError('conflict', 'The plan changed.', 'Review the refreshed revision.')
      );
      render(PlanningView, { workItem });

      await screen.findByText(/Approval Pending/);
      await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
      const editor = screen.getByLabelText('Synthesized plan Markdown');
      await fireEvent.input(editor, { target: { value: '# Unsaved revision' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Save Revision' }));

      expect(editor).toBeInTheDocument();
      expect(editor).toHaveValue('# Unsaved revision');
      expect(screen.getByRole('alert')).toHaveTextContent(
        'The plan changed. Review the refreshed revision.'
      );
  });

  it('edits, saves, approves, rejects, and only enqueues an eligible required plan', async () => {
    const required = planDetail(true);
    const saved = planDetail(true);
    saved.plan!.markdownBody = '# Revised plan';
    saved.plan!.updatedAt = 'saved-version';
    const rejected = planDetail(true);
    rejected.plan!.markdownBody = '# Revised plan';
    rejected.plan!.approvalStatus = 'rejected';
    rejected.plan!.updatedAt = 'rejected-version';
    const approved = planDetail(true);
    approved.plan!.markdownBody = '# Revised plan';
    approved.plan!.approvalStatus = 'approved';
    approved.plan!.updatedAt = 'approved-version';
    approved.currentPhase = 'ready';
    approved.status = 'eligible';
    approved.queue = { state: 'eligible', eligible: true, reason: null, entry: null };

    mocks.getPlanning.mockResolvedValue(required);
    mocks.updateSynthesizedPlan.mockResolvedValue(saved);
    mocks.approvePlan.mockResolvedValue(approved);
    mocks.rejectPlan.mockResolvedValue(rejected);
    mocks.enqueuePlan.mockResolvedValue({
      ...approved,
      currentPhase: 'queue',
      status: 'queued',
      queue: {
        state: 'queued',
        eligible: false,
        reason: null,
        entry: {
          id: 'queue',
          position: 0,
          schedulingStatus: 'queued',
          createdAt: 'created',
          updatedAt: 'updated'
        }
      }
    });
    render(PlanningView, { workItem });

    expect(await screen.findByText(/Approval Pending/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Enqueue' })).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    await fireEvent.input(screen.getByLabelText('Synthesized plan Markdown'), {
      target: { value: '# Revised plan' }
    });
    expect(screen.getByRole('button', { name: 'Approve Plan' })).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Save Revision' }));
    expect(mocks.updateSynthesizedPlan).toHaveBeenCalledWith({
      planningRunId: 'run',
      expectedPlanUpdatedAt: 'plan-version',
      markdownBody: '# Revised plan'
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
    expect(mocks.rejectPlan).toHaveBeenCalledWith({
      planningRunId: 'run',
      expectedPlanUpdatedAt: 'saved-version'
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Approve Plan' }));
    expect(mocks.approvePlan).toHaveBeenCalledWith({
      planningRunId: 'run',
      expectedPlanUpdatedAt: 'rejected-version'
    });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Enqueue' })).toBeEnabled());
    await fireEvent.click(screen.getByRole('button', { name: 'Enqueue' }));
    expect(mocks.enqueuePlan).toHaveBeenCalledWith({ planningRunId: 'run' });
    expect(await screen.findByText('Queued at position 1')).toBeInTheDocument();
  });

  it('allows enqueue without approval when approval is optional', async () => {
    const optional = planDetail(false);
    mocks.getPlanning.mockResolvedValue(optional);
    mocks.enqueuePlan.mockResolvedValue(optional);
    render(PlanningView, {
      workItem: { ...workItem, requirePlanApproval: false }
    });

    expect(await screen.findByText(/Approval not required/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Approve Plan' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Enqueue' })).toBeEnabled();
  });
});
