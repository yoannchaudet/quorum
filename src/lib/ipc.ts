import { invoke } from '@tauri-apps/api/core';
import type { CreateWorkItemRequest } from '../../src-tauri/bindings/CreateWorkItemRequest';
import type { RegisterRepositoryRequest } from '../../src-tauri/bindings/RegisterRepositoryRequest';
import type { RepositoryDto } from '../../src-tauri/bindings/RepositoryDto';
import type { WorkItemDto } from '../../src-tauri/bindings/WorkItemDto';
import type { SettingsDto } from '../../src-tauri/bindings/SettingsDto';
import type { UpdateSettingsRequest } from '../../src-tauri/bindings/UpdateSettingsRequest';

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
  getWorkItem: (workItemId: string) => command<WorkItemDto>('get_work_item', { workItemId }),
  getSettings: () => command<SettingsDto>('get_settings'),
  updateSettings: (request: UpdateSettingsRequest) =>
    command<SettingsDto>('update_settings', { request }),
  listCopilotModels: () => command<string[]>('list_copilot_models')
};
