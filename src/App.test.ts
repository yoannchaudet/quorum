import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App.svelte';

const mocks = vi.hoisted(() => ({ listRepositories: vi.fn() }));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
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

describe('Quorum shell', () => {
  beforeEach(() => {
    mocks.listRepositories.mockReset();
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
});
