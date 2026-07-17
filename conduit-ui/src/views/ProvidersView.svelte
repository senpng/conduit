<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { providers as providersApi } from "../lib/consoleClient";
  import type { Provider } from "../lib/consoleClient";
  import { fmtDate } from "../lib/format";
  import Modal from "../components/Modal.svelte";
  import OAuthPanel from "./OAuthPanel.svelte";

  let list = $state<Provider[]>([]);
  let loading = $state(true);
  let showForm = $state(false);
  let showOAuth = $state(false);
  let secretFor = $state<Provider | null>(null);
  let secretValue = $state("");
  let editing = $state<Provider | null>(null);
  let editForm = $state({ name: "", base_url: "" });

  let form = $state({
    name: "",
    kind: "openai",
    base_url: "https://api.openai.com",
    api_key: "",
  });

  const kindDefaults: Record<string, string> = {
    openai: "https://api.openai.com",
    anthropic: "https://api.anthropic.com",
    "claude-oauth": "https://api.anthropic.com",
    "codex-oauth": "https://chatgpt.com/backend-api/codex",
    "grok-oauth": "https://cli-chat-proxy.grok.com/v1",
    other: "",
  };

  async function load() {
    loading = true;
    try {
      list = await providersApi.list();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("providers", () => void load());
  });
  onDestroy(() => unregister?.());

  function onKindChange() {
    const d = kindDefaults[form.kind];
    if (d !== undefined) form.base_url = d;
  }

  async function create() {
    if (!form.name.trim()) {
      app.toast("Name is required", "warn");
      return;
    }
    try {
      await providersApi.create({
        name: form.name.trim(),
        kind: form.kind,
        base_url: form.base_url.trim(),
        api_key: form.api_key || undefined,
      });
      showForm = false;
      form = { name: "", kind: "openai", base_url: kindDefaults.openai, api_key: "" };
      app.toast("Provider created", "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function remove(p: Provider) {
    const ok = await app.askConfirm({
      title: `Delete provider "${p.name}"?`,
      body: `id ${p.id} · kind ${p.kind}. This cannot be undone.`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await providersApi.delete(p.id);
      app.toast(`Provider "${p.name}" deleted`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function saveSecret() {
    if (!secretFor || !secretValue.trim()) return;
    const p = secretFor;
    const ok = await app.askConfirm({
      title: `Rotate upstream secret for "${p.name}"?`,
      body: `The stored credential for id ${p.id} will be overwritten.`,
      confirmLabel: "Overwrite",
      danger: true,
    });
    if (!ok) return;
    try {
      await providersApi.setSecret(p.id, secretValue.trim());
      secretFor = null;
      secretValue = "";
      app.toast("Secret updated", "ok");
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  function startEdit(p: Provider) {
    editing = p;
    editForm = { name: p.name, base_url: p.base_url };
  }

  async function saveEdit() {
    if (!editing) return;
    try {
      await providersApi.update(editing.id, {
        name: editForm.name.trim() || undefined,
        base_url: editForm.base_url.trim() || undefined,
      });
      editing = null;
      app.toast("Provider updated", "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }
</script>

<div class="panel">
  <div class="row-between">
    <span class="panel-title">Upstream providers</span>
    <div class="row-gap">
      <button class="btn-ghost" onclick={() => (showOAuth = true)}>OAuth login</button>
      <button class="btn-primary" onclick={() => (showForm = !showForm)}>
        {showForm ? "Close form" : "＋ Add provider"}
      </button>
    </div>
  </div>

  {#if showForm}
    <div class="form-card">
      <h3>New provider (API key)</h3>
      <div class="form-grid">
        <label>
          Name
          <input bind:value={form.name} placeholder="My OpenAI account" />
        </label>
        <label>
          Kind
          <select bind:value={form.kind} onchange={onKindChange}>
            <option value="openai">OpenAI</option>
            <option value="anthropic">Anthropic</option>
            <option value="other">Other (OpenAI-compatible)</option>
          </select>
        </label>
        <label>
          Base URL
          <input bind:value={form.base_url} placeholder="https://api.openai.com" />
        </label>
        <label>
          API key (optional)
          <input
            type="password"
            bind:value={form.api_key}
            placeholder="sk-… stored in secret backend"
            autocomplete="off"
          />
          <span class="field-hint">Sent as <code>api_key</code>; held by the OS keychain / master-password store.</span>
        </label>
      </div>
      <div class="form-actions">
        <button class="btn-primary" onclick={create}>Create</button>
        <button class="btn-ghost" onclick={() => (showForm = false)}>Cancel</button>
      </div>
    </div>
  {/if}

  {#if loading && list.length === 0}
    <div class="loader"><span class="spinner"></span></div>
  {:else}
    <div class="table-wrap">
      <table class="table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Kind</th>
            <th>Base URL</th>
            <th>Key ref</th>
            <th>Created</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each list as p (p.id)}
            <tr>
              <td>{p.name}</td>
              <td><span class="badge accent">{p.kind}</span></td>
              <td class="mono dim small">{p.base_url}</td>
              <td class="mono muted tiny">{p.upstream_key_ref}</td>
              <td class="dim small">{fmtDate(p.created_at)}</td>
              <td class="actions">
                <button class="btn-icon" title="Edit name / base URL" onclick={() => startEdit(p)}>✎</button>
                <button
                  class="btn-icon"
                  title="Rotate API key"
                  onclick={() => {
                    secretFor = p;
                    secretValue = "";
                  }}>⚿</button
                >
                <button class="btn-icon danger" title="Delete" onclick={() => void remove(p)}>✕</button>
              </td>
            </tr>
          {:else}
            <tr><td colspan="6" class="empty">No providers yet — add one or use OAuth login.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

{#if showOAuth}
  <OAuthPanel
    ondone={() => {
      showOAuth = false;
      void load();
    }}
  />
{/if}

{#if secretFor}
  <Modal onclose={() => (secretFor = null)}>
    <h3>Rotate API key — {secretFor.name}</h3>
    <p class="modal-hint">
      Stored via the secret backend. Body field is <code>api_key</code> (daemon
      contract). Overwrite requires confirmation.
    </p>
    <input
      type="password"
      bind:value={secretValue}
      placeholder="sk-…"
      autocomplete="off"
    />
    <div class="form-actions">
      <button class="btn-danger" disabled={!secretValue.trim()} onclick={saveSecret}>
        Overwrite secret
      </button>
      <button class="btn-ghost" onclick={() => (secretFor = null)}>Cancel</button>
    </div>
  </Modal>
{/if}

{#if editing}
  <Modal onclose={() => (editing = null)}>
    <h3>Edit provider — {editing.name}</h3>
    <div class="form-grid">
      <label>Name <input bind:value={editForm.name} /></label>
      <label>Base URL <input bind:value={editForm.base_url} /></label>
    </div>
    <div class="form-actions">
      <button class="btn-primary" onclick={saveEdit}>Save</button>
      <button class="btn-ghost" onclick={() => (editing = null)}>Cancel</button>
    </div>
  </Modal>
{/if}
