<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { onMount } from 'svelte';
  import { api, IpcError } from './lib/ipc';
  import { renderMarkdown } from './lib/markdown';
  import type { RepositoryDto } from '../src-tauri/bindings/RepositoryDto';
  import type { WorkItemDto } from '../src-tauri/bindings/WorkItemDto';

  let repositories: RepositoryDto[] = [];
  let selectedRepository: RepositoryDto | null = null;
  let workItems: WorkItemDto[] = [];
  let selectedWorkItem: WorkItemDto | null = null;
  let loading = true;
  let error = '';
  let startupError = false;
  let showAddRepository = false;
  let showArchiveConfirmation = false;
  let showNewWorkItem = false;
  let manualPath = '';
  let title = '';
  let markdownBody = '';

  const message = (cause: unknown) =>
    cause instanceof IpcError ? `${cause.message}${cause.recovery ? ` ${cause.recovery}` : ''}` : 'An unexpected error occurred.';

  async function reloadRepositories() {
    loading = true;
    error = '';
    startupError = false;
    try {
      repositories = await api.listRepositories();
      if (selectedRepository && !repositories.some((repository) => repository.id === selectedRepository?.id)) {
        selectedRepository = null;
        selectedWorkItem = null;
        workItems = [];
      }
    } catch (cause) {
      error = message(cause);
      startupError = true;
    } finally {
      loading = false;
    }
  }

  async function selectRepository(repository: RepositoryDto) {
    selectedRepository = repository;
    selectedWorkItem = null;
    workItems = [];
    error = '';
    try {
      workItems = await api.listWorkItems(repository.id);
    } catch (cause) {
      error = message(cause);
    }
  }

  async function register(path: string) {
    if (!path.trim()) return;
    try {
      const repository = await api.registerRepository({ path: path.trim() });
      showAddRepository = false;
      manualPath = '';
      await reloadRepositories();
      await selectRepository(repository);
    } catch (cause) {
      error = message(cause);
    }
  }

  async function chooseFolder() {
    const selection = await open({ directory: true, multiple: false, title: 'Add Git Repository' });
    if (typeof selection === 'string') await register(selection);
  }

  async function archiveSelected() {
    if (!selectedRepository) return;
    try {
      await api.archiveRepository(selectedRepository.id);
      showArchiveConfirmation = false;
      await reloadRepositories();
    } catch (cause) {
      error = message(cause);
    }
  }

  async function createWorkItem() {
    if (!selectedRepository || !title.trim()) return;
    try {
      const item = await api.createWorkItem({
        repositoryId: selectedRepository.id,
        title: title.trim(),
        markdownBody
      });
      workItems = [item, ...workItems];
      selectedWorkItem = item;
      title = '';
      markdownBody = '';
      showNewWorkItem = false;
    } catch (cause) {
      error = message(cause);
    }
  }

  function externalMarkdownLinks(node: HTMLElement) {
    const handleClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const link = target.closest<HTMLAnchorElement>('a[href]');
      if (!link) return;
      event.preventDefault();
      void openUrl(link.href).catch((cause) => {
        error = message(cause);
      });
    };
    node.addEventListener('click', handleClick);
    return {
      destroy: () => node.removeEventListener('click', handleClick)
    };
  }

  onMount(reloadRepositories);
</script>

<svelte:head><title>Quorum</title></svelte:head>

<main aria-label="Quorum workspace">
  <header data-tauri-drag-region class="titlebar"><span>Quorum</span></header>
  <aside aria-label="Sources">
    <div class="section-heading"><span>Repositories</span><button aria-label="Add repository" title="Add repository" on:click={() => (showAddRepository = true)}>+</button></div>
    {#if loading}<p class="muted">Loading…</p>{/if}
    {#each repositories as repository (repository.id)}
      <button class:selected={selectedRepository?.id === repository.id} class="source-row" on:click={() => selectRepository(repository)}>{repository.displayName}</button>
    {/each}
    {#if !loading && repositories.length === 0}<p class="muted">No repositories</p>{/if}
    {#if selectedRepository}
      <div class="section-heading work-heading"><span>Work Items</span><button aria-label="New work item" title="New work item" on:click={() => (showNewWorkItem = true)}>+</button></div>
      {#each workItems as item (item.id)}
        <button class:selected={selectedWorkItem?.id === item.id} class="source-row work-item" on:click={() => (selectedWorkItem = item)}>{item.title}</button>
      {/each}
      {#if workItems.length === 0}<p class="muted">No work items</p>{/if}
    {/if}
  </aside>
  <section class="detail" aria-live="polite">
    {#if error}<div role="alert" class="error"><span>{error}</span><button on:click={reloadRepositories}>Retry</button></div>{/if}
    {#if selectedWorkItem}
      <div class="toolbar"><span>{selectedRepository?.displayName}</span><button on:click={() => (showArchiveConfirmation = true)}>Archive repository</button></div>
      <article><h1>{selectedWorkItem.title}</h1><div class="markdown" use:externalMarkdownLinks>{@html renderMarkdown(selectedWorkItem.markdownBody)}</div></article>
    {:else if selectedRepository}
      <div class="toolbar"><span>{selectedRepository.displayName}</span><button on:click={() => (showNewWorkItem = true)}>New Work Item</button><button on:click={() => (showArchiveConfirmation = true)}>Archive</button></div>
      <div class="empty"><h1>No work selected</h1><p>Create an inline Markdown work item to begin.</p><button class="primary" on:click={() => (showNewWorkItem = true)}>New Work Item</button></div>
    {:else if startupError}
      <div class="empty startup-error" role="alert"><h1>Quorum can’t open its data</h1><p>{error}</p><p>Your data has not been changed. Restore access to Quorum’s application data, then retry.</p></div>
    {:else}
      <div class="empty"><h1>Welcome to Quorum</h1><p>Add a local Git repository to keep its work items organized.</p><button class="primary" on:click={() => (showAddRepository = true)}>Add Repository</button></div>
    {/if}
  </section>
</main>

{#if showAddRepository}
  <div class="scrim" role="presentation"><dialog open class="sheet" aria-labelledby="add-repository-title"><h2 id="add-repository-title">Add Repository</h2><p>Choose a local Git repository.</p><button class="primary" on:click={chooseFolder}>Choose Folder…</button><details><summary>Enter a path manually</summary><label>Repository path <input bind:value={manualPath} placeholder="/Users/me/Projects/repository" /></label><button on:click={() => register(manualPath)}>Add Path</button></details><button class="cancel" on:click={() => (showAddRepository = false)}>Cancel</button></dialog></div>
{/if}
{#if showArchiveConfirmation}
  <div class="scrim" role="presentation"><dialog open class="sheet" aria-labelledby="archive-title"><h2 id="archive-title">Archive repository?</h2><p>Its work items stay safely in Quorum and can be restored by adding this repository again.</p><button class="danger" on:click={archiveSelected}>Archive</button><button class="cancel" on:click={() => (showArchiveConfirmation = false)}>Cancel</button></dialog></div>
{/if}
{#if showNewWorkItem}
  <div class="scrim" role="presentation"><dialog open class="sheet work-sheet" aria-labelledby="new-work-title"><h2 id="new-work-title">New Work Item</h2><label>Title <input bind:value={title} /></label><label>Markdown <textarea bind:value={markdownBody} rows="10" placeholder="Describe the work…"></textarea></label><button class="primary" disabled={!title.trim()} on:click={createWorkItem}>Create Work Item</button><button class="cancel" on:click={() => (showNewWorkItem = false)}>Cancel</button></dialog></div>
{/if}
