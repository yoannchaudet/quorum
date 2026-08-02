import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App.svelte';

const mocks = vi.hoisted(() => ({
  listRepositories: vi.fn(),
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  listCopilotModels: vi.fn(),
  revealItemInDir: vi.fn()
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn(),
  revealItemInDir: mocks.revealItemInDir
}));
vi.mock('./lib/ipc', () => ({
  IpcError: class IpcError extends Error {},
  api: {
    listRepositories: mocks.listRepositories,
    registerRepository: vi.fn(),
    archiveRepository: vi.fn(),
    listWorkItems: vi.fn(),
    createWorkItem: vi.fn(),
    getWorkItem: vi.fn(),
    getSettings: mocks.getSettings,
    updateSettings: mocks.updateSettings,
    listCopilotModels: mocks.listCopilotModels
  }
}));

describe('Quorum shell', () => {
  beforeEach(() => {
    mocks.listRepositories.mockReset();
    mocks.getSettings.mockReset();
    mocks.updateSettings.mockReset();
    mocks.listCopilotModels.mockReset();
    mocks.revealItemInDir.mockReset();
  });

  it('shows an accessible empty onboarding state', async () => {
    mocks.listRepositories.mockResolvedValue([]);
    render(App);
    await waitFor(() => expect(screen.getByText('Welcome to Quorum')).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Add Repository' })).toBeInTheDocument();
    expect(screen.getByLabelText('Sources')).toBeInTheDocument();
  });

  it('shows a recoverable startup error', async () => {
    mocks.listRepositories.mockRejectedValue(new Error('offline'));
    render(App);
    await waitFor(() => expect(screen.getByText('Quorum can’t open its data')).toBeInTheDocument());
  });

  it('returns to onboarding after a successful retry', async () => {
    mocks.listRepositories.mockRejectedValueOnce(new Error('offline')).mockResolvedValueOnce([]);
    render(App);
    await waitFor(() => expect(screen.getByText('Quorum can’t open its data')).toBeInTheDocument());

    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));

    await waitFor(() => expect(screen.getByText('Welcome to Quorum')).toBeInTheDocument());
    expect(screen.queryByText('Quorum can’t open its data')).not.toBeInTheDocument();
  });

  it('edits model roles and reveals the SQLite database', async () => {
    mocks.listRepositories.mockResolvedValue([]);
    mocks.getSettings.mockResolvedValue({
      databasePath: '/Users/me/Library/Application Support/quorum/quorum.sqlite3',
      planningModels: ['gpt-5.6-sol', 'claude-opus-5'],
      implementationModel: 'gpt-5.6-sol',
      adversaryModel: 'claude-opus-5'
    });
    mocks.listCopilotModels.mockResolvedValue(['gpt-5.6-sol', 'claude-opus-5']);
    mocks.updateSettings.mockResolvedValue({
      databasePath: '/Users/me/Library/Application Support/quorum/quorum.sqlite3',
      planningModels: ['gpt-5.6-sol', 'claude-opus-5', 'custom-planner'],
      implementationModel: 'gpt-5.6-sol',
      adversaryModel: 'claude-opus-5'
    });
    render(App);

    await fireEvent.click(screen.getByRole('button', { name: 'Settings' }));
    await waitFor(() =>
      expect(
        screen.getByText('/Users/me/Library/Application Support/quorum/quorum.sqlite3')
      ).toBeInTheDocument()
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Show in Finder' }));
    expect(mocks.revealItemInDir).toHaveBeenCalledWith(
      '/Users/me/Library/Application Support/quorum/quorum.sqlite3'
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Add planner' }));
    await fireEvent.input(screen.getByLabelText('Planner 3'), {
      target: { value: 'custom-planner' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save Settings' }));

    await waitFor(() =>
      expect(mocks.updateSettings).toHaveBeenCalledWith({
        planningModels: ['gpt-5.6-sol', 'claude-opus-5', 'custom-planner'],
        implementationModel: 'gpt-5.6-sol',
        adversaryModel: 'claude-opus-5'
      })
    );
    expect(screen.getByRole('status')).toHaveTextContent('Settings saved.');
  });
});
