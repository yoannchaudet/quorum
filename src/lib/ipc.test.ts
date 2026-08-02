import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api, IpcError } from './ipc';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));

describe('IPC error normalization', () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
  });

  it('preserves a well-formed structured error', async () => {
    mocks.invoke.mockRejectedValue({
      code: 'validation',
      message: 'Choose a Git repository.',
      recovery: 'Try another folder.'
    });

    await expect(api.listRepositories()).rejects.toEqual(
      new IpcError('validation', 'Choose a Git repository.', 'Try another folder.')
    );
  });

  it('rejects malformed structured errors', async () => {
    mocks.invoke.mockRejectedValue({ code: 42, message: { text: 'broken' } });

    await expect(api.listRepositories()).rejects.toEqual(
      new IpcError('unexpected', 'Quorum could not complete that request. Please try again.')
    );
  });

  it('uses typed settings command names and payloads', async () => {
    mocks.invoke
      .mockResolvedValueOnce({
        databasePath: '/tmp/quorum.sqlite3',
        planningModels: ['planner'],
        implementationModel: 'builder',
        adversaryModel: 'reviewer',
        terminalApplication: 'Ghostty.app',
        terminalArguments:
          '-W -na {terminalApplication} --args -e copilot -C {repositoryPath} --resume={sessionName}'
      })
      .mockResolvedValueOnce(['planner']);

    await api.updateSettings({
      planningModels: ['planner'],
      implementationModel: 'builder',
      adversaryModel: 'reviewer',
      terminalApplication: 'Ghostty.app',
      terminalArguments:
        '-W -na {terminalApplication} --args -e copilot -C {repositoryPath} --resume={sessionName}'
    });
    await api.listCopilotModels();

    expect(mocks.invoke).toHaveBeenNthCalledWith(1, 'update_settings', {
      request: {
        planningModels: ['planner'],
        implementationModel: 'builder',
        adversaryModel: 'reviewer',
        terminalApplication: 'Ghostty.app',
        terminalArguments:
          '-W -na {terminalApplication} --args -e copilot -C {repositoryPath} --resume={sessionName}'
      }
    });
    expect(mocks.invoke).toHaveBeenNthCalledWith(2, 'list_copilot_models', undefined);
  });

  it('uses typed intake and planning commands without accepting terminal paths or sessions', async () => {
    mocks.invoke.mockResolvedValue({});

    await api.intakeInlineMarkdown({
      repositoryId: 'repository',
      title: 'Inline',
      markdownBody: '# Inline',
      requirePlanApproval: true
    });
    await api.intakeLocalMarkdown({
      repositoryId: 'repository',
      path: '/selected/work.md',
      requirePlanApproval: false
    });
    await api.intakeGithubIssue({
      repositoryId: 'repository',
      reference: 'owner/repository#42',
      requirePlanApproval: true
    });
    await api.startPlanning({ workItemId: 'work', idempotencyKey: 'start-key' });
    await api.getPlanning('work');
    await api.submitPlanningAnswers({
      planningRunId: 'run',
      planningAgentId: 'agent',
      expectedRunUpdatedAt: 'version',
      answers: [{ questionId: 'question', answerMarkdown: 'Answer' }]
    });
    await api.retryPlanningAgent({
      planningRunId: 'run',
      planningAgentId: 'agent',
      expectedRunUpdatedAt: 'version'
    });
    await api.openPlanningTerminal({
      workItemId: 'work',
      planningAgentId: 'agent',
      idempotencyKey: 'terminal-key'
    });
    await api.openCopilotSession({
      workItemId: 'work',
      planningAgentId: 'agent'
    });
    await api.reconcilePlanningTerminal({
      workItemId: 'work',
      terminalHandoffId: 'handoff'
    });
    await api.updateSynthesizedPlan({
      planningRunId: 'run',
      expectedPlanUpdatedAt: 'plan-version',
      markdownBody: '# Updated'
    });
    await api.approvePlan({
      planningRunId: 'run',
      expectedPlanUpdatedAt: 'plan-version'
    });
    await api.rejectPlan({
      planningRunId: 'run',
      expectedPlanUpdatedAt: 'plan-version'
    });
    await api.enqueuePlan({ planningRunId: 'run' });

    expect(mocks.invoke.mock.calls).toEqual([
      [
        'intake_inline_markdown',
        {
          request: {
            repositoryId: 'repository',
            title: 'Inline',
            markdownBody: '# Inline',
            requirePlanApproval: true
          }
        }
      ],
      [
        'intake_local_markdown',
        {
          request: {
            repositoryId: 'repository',
            path: '/selected/work.md',
            requirePlanApproval: false
          }
        }
      ],
      [
        'intake_github_issue',
        {
          request: {
            repositoryId: 'repository',
            reference: 'owner/repository#42',
            requirePlanApproval: true
          }
        }
      ],
      ['start_planning', { request: { workItemId: 'work', idempotencyKey: 'start-key' } }],
      ['get_planning', { workItemId: 'work' }],
      [
        'submit_planning_answers',
        {
          request: {
            planningRunId: 'run',
            planningAgentId: 'agent',
            expectedRunUpdatedAt: 'version',
            answers: [{ questionId: 'question', answerMarkdown: 'Answer' }]
          }
        }
      ],
      [
        'retry_planning_agent',
        {
          request: {
            planningRunId: 'run',
            planningAgentId: 'agent',
            expectedRunUpdatedAt: 'version'
          }
        }
      ],
      [
        'open_planning_terminal',
        {
          request: {
            workItemId: 'work',
            planningAgentId: 'agent',
            idempotencyKey: 'terminal-key'
          }
        }
      ],
      [
        'open_copilot_session',
        {
          request: {
            workItemId: 'work',
            planningAgentId: 'agent'
          }
        }
      ],
      [
        'reconcile_planning_terminal',
        { request: { workItemId: 'work', terminalHandoffId: 'handoff' } }
      ],
      [
        'update_synthesized_plan',
        {
          request: {
            planningRunId: 'run',
            expectedPlanUpdatedAt: 'plan-version',
            markdownBody: '# Updated'
          }
        }
      ],
      [
        'approve_plan',
        { request: { planningRunId: 'run', expectedPlanUpdatedAt: 'plan-version' } }
      ],
      [
        'reject_plan',
        { request: { planningRunId: 'run', expectedPlanUpdatedAt: 'plan-version' } }
      ],
      ['enqueue_plan', { request: { planningRunId: 'run' } }]
    ]);
  });
});
