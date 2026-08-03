import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ExecutionView from './ExecutionView.svelte';
import type { ExecutionDetailDto } from './lib/ipc';

const mocks = vi.hoisted(() => ({
  getExecution: vi.fn(),
  startExecution: vi.fn(),
  resumeExecution: vi.fn(),
  cancelExecution: vi.fn(),
  resolveExecutionFinding: vi.fn()
}));

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

function detail(overrides: Partial<ExecutionDetailDto> = {}): ExecutionDetailDto {
  return {
    run: {
      id: 'run',
      workItemId: 'work',
      planId: 'plan',
      queueEntryId: 'queue',
      phase: 'building',
      outcome: 'running',
      status: 'verifying',
      currentStep: 'verifying',
      baseCommit: '0123456789abcdef',
      branchName: 'quorum/implement-fixture-12345678',
      worktreePath: '/app/worktrees/implement-fixture-12345678',
      builderSessionName: 'quorum-implement-fixture-12345678-builder',
      builderModel: 'gpt-5.6-sol',
      reviewerSessionName: 'quorum-implement-fixture-12345678-reviewer',
      reviewerModel: 'claude-opus-5',
      verificationProgram: '/usr/bin/make',
      verificationArguments: ['check'],
      iteration: 0,
      maxIterations: 3,
      errorCode: null,
      errorMessage: null,
      createdAt: 'created',
      updatedAt: 'updated',
      completedAt: null
    },
    attempts: [
      {
        id: 'attempt',
        number: 1,
        reason: 'start',
        status: 'running',
        errorCode: null,
        errorMessage: null,
        startedAt: 'created',
        completedAt: null
      }
    ],
    recentLogs: [
      {
        id: 'log',
        commandId: 'command',
        phase: 'verifying',
        program: '/usr/bin/make',
        stream: 'stdout',
        sequence: 0,
        text: 'all checks passed',
        truncated: false,
        createdAt: 'now'
      }
    ],
    findings: [],
    recentEvents: [
      {
        id: 'event',
        sequence: 2,
        eventKind: 'verification_started',
        payload: { message: 'Running make check' },
        createdAt: 'now'
      }
    ],
    blockingFindingCount: 0,
    canResume: false,
    canCancel: true,
    deliveryReady: false,
    ...overrides
  };
}

describe('execution UX', () => {
  beforeEach(() => {
    for (const mock of Object.values(mocks)) mock.mockReset();
  });

  it('starts one explicit execution from the queued plan', async () => {
    mocks.getExecution.mockResolvedValue(null);
    mocks.startExecution.mockResolvedValue(detail());
    render(ExecutionView, { workItemId: 'work', queueEntryId: 'queue' });

    await fireEvent.click(await screen.findByRole('button', { name: 'Start Execution' }));

    expect(mocks.startExecution).toHaveBeenCalledWith({
      queueEntryId: 'queue',
      idempotencyKey: expect.any(String)
    });
    expect(await screen.findByText('quorum/implement-fixture-12345678')).toBeInTheDocument();
  });

  it('shows durable metadata, bounded output, and targeted cancellation', async () => {
    const running = detail();
    mocks.getExecution.mockResolvedValue(running);
    mocks.cancelExecution.mockResolvedValue({
      ...running,
      run: { ...running.run, status: 'cancelling' },
      canCancel: false
    });
    render(ExecutionView, { workItemId: 'work', queueEntryId: 'queue' });

    expect(await screen.findByText('all checks passed')).toBeInTheDocument();
    expect(screen.getByText('/app/worktrees/implement-fixture-12345678')).toBeInTheDocument();
    expect(screen.getByText('quorum-implement-fixture-12345678-builder')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel Owned Processes' }));
    expect(mocks.cancelExecution).toHaveBeenCalledWith({ runId: 'run' });
  });

  it('requires a disposition note and marks delivery ready after resolution', async () => {
    const blocked = detail({
      run: {
        ...detail().run,
        phase: 'reviewing',
        outcome: 'blocked',
        status: 'blocked',
        currentStep: 'reviewing',
        iteration: 3,
        errorCode: 'blocking_findings',
        errorMessage: 'One blocking finding remains.',
        completedAt: 'completed'
      },
      findings: [
        {
          id: 'finding',
          externalId: 'missing-validation',
          severity: 'blocking',
          title: 'Missing validation',
          body: 'Invalid input is accepted.',
          path: 'src/input.rs',
          line: 12,
          status: 'open',
          dispositionNote: null,
          firstSeenIteration: 0,
          lastSeenIteration: 3,
          createdAt: 'created',
          updatedAt: 'updated',
          resolvedAt: null
        }
      ],
      blockingFindingCount: 1,
      canResume: false,
      canCancel: false
    });
    const ready = {
      ...blocked,
      run: {
        ...blocked.run,
        phase: 'delivery',
        outcome: 'succeeded',
        status: 'ready',
        currentStep: 'complete',
        errorCode: null,
        errorMessage: null
      },
      findings: [
        {
          ...blocked.findings[0],
          status: 'resolved',
          dispositionNote: 'Upstream validation is authoritative.'
        }
      ],
      blockingFindingCount: 0,
      deliveryReady: true
    };
    mocks.getExecution.mockResolvedValue(blocked);
    mocks.resolveExecutionFinding.mockResolvedValue(ready);
    render(ExecutionView, { workItemId: 'work', queueEntryId: 'queue' });

    const resolve = await screen.findByRole('button', { name: 'Resolve Finding' });
    expect(resolve).toBeDisabled();
    await fireEvent.input(screen.getByLabelText('Disposition note'), {
      target: { value: 'Upstream validation is authoritative.' }
    });
    await fireEvent.click(resolve);

    expect(mocks.resolveExecutionFinding).toHaveBeenCalledWith({
      runId: 'run',
      findingId: 'finding',
      dispositionNote: 'Upstream validation is authoritative.'
    });
    await waitFor(() => expect(screen.getByText('Ready for delivery')).toBeInTheDocument());
  });
});
