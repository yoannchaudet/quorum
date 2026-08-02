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
        adversaryModel: 'reviewer'
      })
      .mockResolvedValueOnce(['planner']);

    await api.updateSettings({
      planningModels: ['planner'],
      implementationModel: 'builder',
      adversaryModel: 'reviewer'
    });
    await api.listCopilotModels();

    expect(mocks.invoke).toHaveBeenNthCalledWith(1, 'update_settings', {
      request: {
        planningModels: ['planner'],
        implementationModel: 'builder',
        adversaryModel: 'reviewer'
      }
    });
    expect(mocks.invoke).toHaveBeenNthCalledWith(2, 'list_copilot_models', undefined);
  });
});
