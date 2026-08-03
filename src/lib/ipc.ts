import { invoke } from '@tauri-apps/api/core';
import type { CreateWorkItemRequest } from '../../src-tauri/bindings/CreateWorkItemRequest';
import type { RegisterRepositoryRequest } from '../../src-tauri/bindings/RegisterRepositoryRequest';
import type { RepositoryDto } from '../../src-tauri/bindings/RepositoryDto';
import type { WorkItemDto } from '../../src-tauri/bindings/WorkItemDto';
import type { SettingsDto } from '../../src-tauri/bindings/SettingsDto';
import type { UpdateSettingsRequest } from '../../src-tauri/bindings/UpdateSettingsRequest';
import type { IntakeLocalMarkdownRequest } from '../../src-tauri/bindings/IntakeLocalMarkdownRequest';
import type { IntakeGithubIssueRequest } from '../../src-tauri/bindings/IntakeGithubIssueRequest';
import type { StartPlanningRequest } from '../../src-tauri/bindings/StartPlanningRequest';
import type { ReplanWorkItemRequest } from '../../src-tauri/bindings/ReplanWorkItemRequest';
import type { SubmitPlanningAnswersRequest } from '../../src-tauri/bindings/SubmitPlanningAnswersRequest';
import type { RetryPlanningRequest } from '../../src-tauri/bindings/RetryPlanningRequest';
import type { LaunchTerminalHandoffRequest } from '../../src-tauri/bindings/LaunchTerminalHandoffRequest';
import type { OpenCopilotSessionRequest } from '../../src-tauri/bindings/OpenCopilotSessionRequest';
import type { ResumeTerminalHandoffRequest } from '../../src-tauri/bindings/ResumeTerminalHandoffRequest';
import type { UpdateSynthesizedPlanRequest } from '../../src-tauri/bindings/UpdateSynthesizedPlanRequest';
import type { PlanApprovalRequest } from '../../src-tauri/bindings/PlanApprovalRequest';
import type { EnqueuePlanRequest } from '../../src-tauri/bindings/EnqueuePlanRequest';
import type { PlanningDetailDto } from '../../src-tauri/bindings/PlanningDetailDto';
import type { PlanningAgentDto } from '../../src-tauri/bindings/PlanningAgentDto';
import type { PlanningEventDto } from '../../src-tauri/bindings/PlanningEventDto';
import type { PlanningQuestionDto } from '../../src-tauri/bindings/PlanningQuestionDto';
import type { PlanRevisionDto } from '../../src-tauri/bindings/PlanRevisionDto';
import type { TerminalHandoffSummaryDto } from '../../src-tauri/bindings/TerminalHandoffSummaryDto';
import type { StartExecutionRequest } from '../../src-tauri/bindings/StartExecutionRequest';
import type { ResumeExecutionRequest } from '../../src-tauri/bindings/ResumeExecutionRequest';
import type { CancelExecutionRequest } from '../../src-tauri/bindings/CancelExecutionRequest';
import type { ResolveExecutionFindingRequest } from '../../src-tauri/bindings/ResolveExecutionFindingRequest';
import type { ExecutionDetailDto } from '../../src-tauri/bindings/ExecutionDetailDto';
import type { ExecutionRunDto } from '../../src-tauri/bindings/ExecutionRunDto';
import type { ExecutionAttemptDto } from '../../src-tauri/bindings/ExecutionAttemptDto';
import type { ExecutionLogDto } from '../../src-tauri/bindings/ExecutionLogDto';
import type { ExecutionFindingDto } from '../../src-tauri/bindings/ExecutionFindingDto';
import type { ExecutionPhaseEventDto } from '../../src-tauri/bindings/ExecutionPhaseEventDto';

export type {
  CancelExecutionRequest,
  CreateWorkItemRequest,
  EnqueuePlanRequest,
  ExecutionAttemptDto,
  ExecutionDetailDto,
  ExecutionFindingDto,
  ExecutionLogDto,
  ExecutionPhaseEventDto,
  ExecutionRunDto,
  IntakeGithubIssueRequest,
  IntakeLocalMarkdownRequest,
  LaunchTerminalHandoffRequest,
  OpenCopilotSessionRequest,
  PlanApprovalRequest,
  PlanRevisionDto,
  PlanningAgentDto,
  PlanningDetailDto,
  PlanningEventDto,
  PlanningQuestionDto,
  RegisterRepositoryRequest,
  ReplanWorkItemRequest,
  RepositoryDto,
  ResumeTerminalHandoffRequest,
  RetryPlanningRequest,
  ResolveExecutionFindingRequest,
  ResumeExecutionRequest,
  SettingsDto,
  StartPlanningRequest,
  StartExecutionRequest,
  SubmitPlanningAnswersRequest,
  TerminalHandoffSummaryDto,
  UpdateSettingsRequest,
  UpdateSynthesizedPlanRequest,
  WorkItemDto
};

export class IpcError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly recovery?: string | null
  ) {
    super(message);
    this.name = 'IpcError';
  }
}

type ErrorPayload = { code: string; message: string; recovery?: string | null };

function normalizeError(error: unknown): IpcError {
  if (isErrorPayload(error)) {
    const payload = error;
    return new IpcError(payload.code, payload.message, payload.recovery);
  }
  return new IpcError('unexpected', 'Quorum could not complete that request. Please try again.');
}

function isErrorPayload(error: unknown): error is ErrorPayload {
  if (typeof error !== 'object' || error === null) return false;
  const candidate = error as Record<string, unknown>;
  return (
    typeof candidate.code === 'string' &&
    typeof candidate.message === 'string' &&
    (candidate.recovery === undefined ||
      candidate.recovery === null ||
      typeof candidate.recovery === 'string')
  );
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw normalizeError(error);
  }
}

export const api = {
  listRepositories: () => command<RepositoryDto[]>('list_repositories'),
  registerRepository: (request: RegisterRepositoryRequest) =>
    command<RepositoryDto>('register_repository', { request }),
  archiveRepository: (repositoryId: string) =>
    command<void>('archive_repository', { repositoryId }),
  listWorkItems: (repositoryId: string) =>
    command<WorkItemDto[]>('list_work_items', { repositoryId }),
  createWorkItem: (request: CreateWorkItemRequest) =>
    command<WorkItemDto>('create_work_item', { request }),
  intakeInlineMarkdown: (request: CreateWorkItemRequest) =>
    command<WorkItemDto>('intake_inline_markdown', { request }),
  intakeLocalMarkdown: (request: IntakeLocalMarkdownRequest) =>
    command<WorkItemDto>('intake_local_markdown', { request }),
  intakeGithubIssue: (request: IntakeGithubIssueRequest) =>
    command<WorkItemDto>('intake_github_issue', { request }),
  getWorkItem: (workItemId: string) => command<WorkItemDto>('get_work_item', { workItemId }),
  getSettings: () => command<SettingsDto>('get_settings'),
  updateSettings: (request: UpdateSettingsRequest) =>
    command<SettingsDto>('update_settings', { request }),
  listCopilotModels: () => command<string[]>('list_copilot_models'),
  startPlanning: (request: StartPlanningRequest) =>
    command<PlanningDetailDto>('start_planning', { request }),
  replanWorkItem: (request: ReplanWorkItemRequest) =>
    command<PlanningDetailDto>('replan_work_item', { request }),
  getPlanning: (workItemId: string) =>
    command<PlanningDetailDto>('get_planning', { workItemId }),
  submitPlanningAnswers: (request: SubmitPlanningAnswersRequest) =>
    command<PlanningDetailDto>('submit_planning_answers', { request }),
  retryPlanningAgent: (request: RetryPlanningRequest) =>
    command<PlanningDetailDto>('retry_planning_agent', { request }),
  openPlanningTerminal: (request: LaunchTerminalHandoffRequest) =>
    command<PlanningDetailDto>('open_planning_terminal', { request }),
  openCopilotSession: (request: OpenCopilotSessionRequest) =>
    command<void>('open_copilot_session', { request }),
  reconcilePlanningTerminal: (request: ResumeTerminalHandoffRequest) =>
    command<PlanningDetailDto>('reconcile_planning_terminal', { request }),
  updateSynthesizedPlan: (request: UpdateSynthesizedPlanRequest) =>
    command<PlanningDetailDto>('update_synthesized_plan', { request }),
  approvePlan: (request: PlanApprovalRequest) =>
    command<PlanningDetailDto>('approve_plan', { request }),
  rejectPlan: (request: PlanApprovalRequest) =>
    command<PlanningDetailDto>('reject_plan', { request }),
  enqueuePlan: (request: EnqueuePlanRequest) =>
    command<PlanningDetailDto>('enqueue_plan', { request }),
  startExecution: (request: StartExecutionRequest) =>
    command<ExecutionDetailDto>('start_execution', { request }),
  getExecution: (workItemId: string) =>
    command<ExecutionDetailDto | null>('get_execution', { workItemId }),
  resumeExecution: (request: ResumeExecutionRequest) =>
    command<ExecutionDetailDto>('resume_execution', { request }),
  cancelExecution: (request: CancelExecutionRequest) =>
    command<ExecutionDetailDto>('cancel_execution', { request }),
  resolveExecutionFinding: (request: ResolveExecutionFindingRequest) =>
    command<ExecutionDetailDto>('resolve_execution_finding', { request })
};
