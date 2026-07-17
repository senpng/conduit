<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { keys as keysApi } from "../lib/adminClient";
  import type { DownstreamKey, CreateKeyResponse } from "../lib/adminClient";
  import { fmtDate } from "../lib/format";
  import KeyCreatedModal from "../components/KeyCreatedModal.svelte";

  let list = $state<DownstreamKey[]>([]);
  let loading = $state(true);
  let showForm = $state(false);
  let reveal = $state<CreateKeyResponse | null>(null);
  let form = $state({ name: "", rate_limit_rpm: "" });

  async function load() {
    loading = true;
    try {
      list = await keysApi.list();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("keys", () => void load());
  });
  onDestroy(() => unregister?.());

  async function create() {
    if (!form.name.trim()) {
      app.toast("Name is required", "warn");
      return;
    }
    try {
      const result = await keysApi.create({
        name: form.name.trim(),
        rate_limit_rpm: form.rate_limit_rpm ? Number(form.rate_limit_rpm) : undefined,
      });
      reveal = result;
      showForm = false;
      form = { name: "", rate_limit_rpm: "" };
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function toggleEnabled(k: DownstreamKey) {
    try {
      await keysApi.update(k.id, { enabled: !k.enabled });
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function remove(k: DownstreamKey) {
    const ok = await app.askConfirm({
      title: `Revoke key "${k.name}"?`,
      body: `id ${k.id}. Clients using this key will lose access immediately. This cannot be undone.`,
      confirmLabel: "Revoke",
    });
    if (!ok) return;
    try {
      await keysApi.delete(k.id);
      app.toast(`Key "${k.name}" revoked`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }
</script>

<div class="panel">
  <div class="row-between">
    <span class="panel-title">Downstream keys</span>
    <button class="btn-primary" onclick={() => (showForm = !showForm)}>
      {showForm ? "Close form" : "＋ Create key"}
    </button>
  </div>

  {#if showForm}
    <div class="form-card">
      <h3>New downstream key</h3>
      <div class="form-grid">
        <label>Name <input bind:value={form.name} placeholder="My app" /></label>
        <label>
          Rate limit (RPM)
          <input type="number" bind:value={form.rate_limit_rpm} placeholder="60" min="0" />
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
            <th>RPM</th>
            <th>Status</th>
            <th>Created</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each list as k (k.id)}
            <tr>
              <td>{k.name}</td>
              <td class="mono">{k.rate_limit_rpm ?? "—"}</td>
              <td>
                <button
                  class="pill"
                  class:on={k.enabled}
                  class:off={!k.enabled}
                  style="border:none; cursor:pointer"
                  title="Toggle enabled"
                  onclick={() => void toggleEnabled(k)}
                >
                  {k.enabled ? "active" : "disabled"}
                </button>
              </td>
              <td class="dim small">{fmtDate(k.created_at)}</td>
              <td class="actions">
                <button class="btn-icon danger" title="Revoke" onclick={() => void remove(k)}>✕</button>
              </td>
            </tr>
          {:else}
            <tr><td colspan="6" class="empty">No keys yet.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

{#if reveal}
  <KeyCreatedModal keyData={reveal} onclose={() => (reveal = null)} />
{/if}
