import { screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ listRepositories: vi.fn() }));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));
vi.mock('./lib/ipc', () => ({
  IpcError: class IpcError extends Error {},
  api: {
    listRepositories: mocks.listRepositories,
    registerRepository: vi.fn(),
    archiveRepository: vi.fn(),
    listWorkItems: vi.fn(),
    createWorkItem: vi.fn(),
    getWorkItem: vi.fn()
  }
}));

describe('application entry point', () => {
  beforeEach(() => {
    vi.resetModules();
    mocks.listRepositories.mockReset();
    document.body.innerHTML = '<div id="app"></div>';
  });

  it('mounts the Svelte 5 application', async () => {
    mocks.listRepositories.mockResolvedValue([]);

    await import('./main');

    await waitFor(() => expect(screen.getByText('Welcome to Quorum')).toBeInTheDocument());
  });
});
