<script lang="ts">
  import { revealItemInDir } from '@tauri-apps/plugin-opener';
  import { onMount } from 'svelte';
  import { api, IpcError } from './lib/ipc';

  let databasePath = '';
  let planningModels: string[] = [];
  let implementationModel = '';
  let adversaryModel = '';
  let availableModels: string[] = [];
  let loading = true;
  let saving = false;
  let error = '';
  let discoveryError = '';
  let saved = false;

  const message = (cause: unknown) =>
    cause instanceof IpcError
      ? `${cause.message}${cause.recovery ? ` ${cause.recovery}` : ''}`
      : 'An unexpected error occurred.';

  async function loadSettings() {
    loading = true;
    error = '';
    try {
      const settings = await api.getSettings();
      databasePath = settings.databasePath;
      planningModels = [...settings.planningModels];
      implementationModel = settings.implementationModel;
      adversaryModel = settings.adversaryModel;
    } catch (cause) {
      error = message(cause);
    } finally {
      loading = false;
    }
  }

  async function discoverModels() {
    discoveryError = '';
    try {
      availableModels = await api.listCopilotModels();
    } catch (cause) {
      discoveryError = message(cause);
    }
  }

  async function save() {
    saved = false;
    error = '';
    saving = true;
    try {
      const settings = await api.updateSettings({
        planningModels: planningModels.map((model) => model.trim()),
        implementationModel: implementationModel.trim(),
        adversaryModel: adversaryModel.trim()
      });
      planningModels = [...settings.planningModels];
      implementationModel = settings.implementationModel;
      adversaryModel = settings.adversaryModel;
      saved = true;
    } catch (cause) {
      error = message(cause);
    } finally {
      saving = false;
    }
  }

  async function revealDatabase() {
    error = '';
    try {
      await revealItemInDir(databasePath);
    } catch (cause) {
      error = message(cause);
    }
  }

  function addPlanner() {
    if (planningModels.length < 3) planningModels = [...planningModels, ''];
  }

  function removePlanner(index: number) {
    if (planningModels.length > 1) {
      planningModels = planningModels.filter((_, modelIndex) => modelIndex !== index);
    }
  }

  const valid = () =>
    planningModels.length >= 1 &&
    planningModels.length <= 3 &&
    planningModels.every((model) => model.trim() !== '') &&
    implementationModel.trim() !== '' &&
    adversaryModel.trim() !== '';

  onMount(() => {
    void loadSettings();
    void discoverModels();
  });
</script>

<div class="settings-toolbar"><span>Settings</span></div>
<div class="settings-page">
  <header>
    <h1>Settings</h1>
    <p>Configure Quorum’s local data and Copilot model roles.</p>
  </header>

  {#if loading}
    <p class="status">Loading settings…</p>
  {:else}
    {#if error}<div role="alert" class="settings-error">{error}</div>{/if}

    <section aria-labelledby="data-heading">
      <h2 id="data-heading">Data</h2>
      <p class="description">Quorum keeps its durable state outside your repositories.</p>
      <div class="path-row">
        <code>{databasePath}</code>
        <button disabled={!databasePath} on:click={revealDatabase}>Show in Finder</button>
      </div>
    </section>

    <section aria-labelledby="models-heading">
      <h2 id="models-heading">Copilot models</h2>
      <p class="description">Discovered models are suggestions. Custom model identifiers are also accepted.</p>
      {#if discoveryError}
        <div role="alert" class="discovery-warning">
          <span>{discoveryError}</span>
          <button on:click={discoverModels}>Retry discovery</button>
        </div>
      {/if}

      <datalist id="copilot-models">
        {#each availableModels as model}<option value={model}></option>{/each}
      </datalist>

      <fieldset>
        <legend>Planning models</legend>
        <p class="description">Quorum can ask one to three independent planners.</p>
        {#each planningModels as _, index (index)}
          <div class="model-row">
            <label for={`planner-${index}`}>Planner {index + 1}</label>
            <input id={`planner-${index}`} list="copilot-models" bind:value={planningModels[index]} />
            <button
              class="remove"
              aria-label={`Remove planner ${index + 1}`}
              disabled={planningModels.length === 1}
              on:click={() => removePlanner(index)}>Remove</button>
          </div>
        {/each}
        <button class="add" disabled={planningModels.length === 3} on:click={addPlanner}>Add planner</button>
      </fieldset>

      <div class="model-row single">
        <label for="implementation-model">Implementation model</label>
        <input id="implementation-model" list="copilot-models" bind:value={implementationModel} />
      </div>
      <div class="model-row single">
        <label for="adversary-model">Adversary review model</label>
        <input id="adversary-model" list="copilot-models" bind:value={adversaryModel} />
      </div>
    </section>

    <footer>
      {#if saved}<span role="status">Settings saved.</span>{/if}
      <button class="primary" disabled={!valid() || saving} on:click={save}>
        {saving ? 'Saving…' : 'Save Settings'}
      </button>
    </footer>
  {/if}
</div>

<style>
  .settings-toolbar {
    position: sticky;
    z-index: 5;
    top: 0;
    display: flex;
    align-items: center;
    min-height: 44px;
    padding: 6px 18px;
    border-bottom: 1px solid var(--hairline);
    background: rgba(247, 247, 248, 0.9);
    backdrop-filter: blur(20px);
  }

  .settings-toolbar span {
    color: var(--secondary);
    font-size: 12px;
  }

  .settings-page {
    width: min(720px, calc(100% - 64px));
    margin: 0 auto;
    padding: 38px 0 64px;
  }

  header h1 {
    margin: 0;
    font-size: 30px;
    letter-spacing: -0.035em;
  }

  header p,
  .description,
  .status {
    color: var(--secondary);
    font-size: 13px;
  }

  section {
    margin-top: 34px;
    padding-top: 22px;
    border-top: 1px solid var(--hairline);
  }

  h2 {
    margin: 0 0 5px;
    font-size: 17px;
  }

  .description {
    margin: 0 0 16px;
    line-height: 1.4;
  }

  .path-row,
  .model-row,
  footer,
  .discovery-warning {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  code {
    min-width: 0;
    flex: 1;
    padding: 8px 10px;
    overflow: hidden;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: white;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  button {
    padding: 5px 10px;
    border: 1px solid rgba(0, 0, 0, 0.14);
    border-radius: 6px;
    color: #303034;
    background: white;
    font-size: 12px;
  }

  button:disabled {
    opacity: 0.45;
  }

  fieldset {
    margin: 22px 0;
    padding: 0;
    border: 0;
  }

  legend {
    padding: 0;
    font-size: 14px;
    font-weight: 600;
  }

  fieldset .description {
    margin-top: 4px;
  }

  .model-row {
    margin: 10px 0;
  }

  .model-row label {
    width: 160px;
    color: #4b4b50;
    font-size: 13px;
  }

  .model-row input {
    min-width: 0;
    flex: 1;
    padding: 7px 9px;
    border: 1px solid rgba(0, 0, 0, 0.19);
    border-radius: 6px;
    background: white;
    font-size: 13px;
  }

  .model-row .remove {
    width: 68px;
  }

  .single {
    margin: 15px 0;
  }

  .single::after {
    width: 68px;
    content: "";
  }

  .discovery-warning,
  .settings-error {
    margin: 12px 0;
    padding: 9px 11px;
    border: 1px solid rgba(180, 110, 0, 0.22);
    border-radius: 7px;
    color: #704800;
    background: #fff7e5;
    font-size: 12px;
  }

  .discovery-warning span {
    flex: 1;
  }

  .settings-error {
    color: #8a0712;
    background: #fff0f1;
    border-color: rgba(215, 0, 21, 0.18);
  }

  footer {
    justify-content: flex-end;
    margin-top: 30px;
  }

  footer span {
    color: #287a38;
    font-size: 12px;
  }

  footer .primary {
    padding: 6px 13px;
    color: white;
    background: var(--accent);
    font-size: 13px;
  }
</style>
