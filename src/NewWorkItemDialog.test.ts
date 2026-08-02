import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import NewWorkItemDialog from './NewWorkItemDialog.svelte';

const mocks = vi.hoisted(() => ({
  open: vi.fn(),
  inline: vi.fn(),
  local: vi.fn(),
  github: vi.fn()
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: mocks.open }));
vi.mock('./lib/ipc', () => ({
  IpcError: class IpcError extends Error {},
  api: {
    intakeInlineMarkdown: mocks.inline,
    intakeLocalMarkdown: mocks.local,
    intakeGithubIssue: mocks.github
  }
}));

const repository = {
  id: 'repository',
  rootPath: '/repo',
  displayName: 'repo',
  createdAt: 'created',
  updatedAt: 'updated'
};

const item = {
  id: 'work',
  repositoryId: 'repository',
  title: 'Work',
  sourceKind: 'inline_markdown',
  markdownBody: '# Work',
  lifecycleStatus: 'open',
  requirePlanApproval: true,
  createdAt: 'created',
  updatedAt: 'updated'
};

describe('new work item intake', () => {
  beforeEach(() => {
    mocks.open.mockReset();
    mocks.inline.mockReset();
    mocks.local.mockReset();
    mocks.github.mockReset();
    mocks.inline.mockResolvedValue(item);
    mocks.local.mockResolvedValue({ ...item, sourceKind: 'local_markdown' });
    mocks.github.mockResolvedValue({ ...item, sourceKind: 'github_issue' });
  });

  it('creates inline Markdown with approval required by default', async () => {
    const onCreated = vi.fn();
    render(NewWorkItemDialog, { repository, onCreated, onClose: vi.fn() });

    await fireEvent.input(screen.getByLabelText('Title'), { target: { value: 'Inline work' } });
    await fireEvent.input(screen.getByLabelText('Markdown'), {
      target: { value: '# Requirements' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Create Work Item' }));

    await waitFor(() =>
      expect(mocks.inline).toHaveBeenCalledWith({
        repositoryId: 'repository',
        title: 'Inline work',
        markdownBody: '# Requirements',
        requirePlanApproval: true
      })
    );
    expect(onCreated).toHaveBeenCalled();
  });

  it('creates a selected local .md file with optional approval', async () => {
    mocks.open.mockResolvedValue('/repo/work.md');
    render(NewWorkItemDialog, { repository, onCreated: vi.fn(), onClose: vi.fn() });

    await fireEvent.click(screen.getByLabelText('Local .md file'));
    await fireEvent.click(screen.getByRole('button', { name: 'Choose .md File…' }));
    expect(mocks.open).toHaveBeenCalledWith(
      expect.objectContaining({ filters: [{ name: 'Markdown', extensions: ['md'] }] })
    );
    expect(await screen.findByText('/repo/work.md')).toBeInTheDocument();
    await fireEvent.click(screen.getByLabelText('Require plan approval before enqueue'));
    await fireEvent.click(screen.getByRole('button', { name: 'Create Work Item' }));

    await waitFor(() =>
      expect(mocks.local).toHaveBeenCalledWith({
        repositoryId: 'repository',
        path: '/repo/work.md',
        requirePlanApproval: false
      })
    );
  });

  it('accepts a GitHub issue URL or owner/repo#number', async () => {
    render(NewWorkItemDialog, { repository, onCreated: vi.fn(), onClose: vi.fn() });

    await fireEvent.click(screen.getByLabelText('GitHub issue'));
    const create = screen.getByRole('button', { name: 'Create Work Item' });
    expect(create).toBeDisabled();
    await fireEvent.input(screen.getByLabelText('GitHub issue URL or owner/repo#number'), {
      target: { value: 'https://github.com/owner/repo/issues/42' }
    });
    expect(create).toBeEnabled();
    await fireEvent.click(create);

    await waitFor(() =>
      expect(mocks.github).toHaveBeenCalledWith({
        repositoryId: 'repository',
        reference: 'https://github.com/owner/repo/issues/42',
        requirePlanApproval: true
      })
    );
  });
});
