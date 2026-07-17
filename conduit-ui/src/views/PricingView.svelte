<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { pricing as pricingApi } from "../lib/consoleClient";
  import type { PricingRow } from "../lib/consoleClient";

  let list = $state<PricingRow[]>([]);
  let loading = $state(true);
  let reloading = $state(false);
  let syncing = $state(false);

  async function load() {
    loading = true;
    try {
      list = await pricingApi.list();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  async function reload() {
    reloading = true;
    try {
      await pricingApi.reload();
      app.toast("pricing layers reloaded", "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      reloading = false;
    }
  }

  async function syncLiteLlm() {
    syncing = true;
    try {
      const r = await pricingApi.sync();
      const n = r.source_models ?? 0;
      const total = r.total_rows ?? list.length;
      app.toast(`LiteLLM synced · ${n} models → ${total} rows`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      syncing = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("pricing", () => void load());
  });
  onDestroy(() => unregister?.());

  const money = (v: number | null) => (v != null ? `$${v.toFixed(2)}` : "—");
</script>

<div class="panel">
  <div class="row-between">
    <span class="panel-title">Model pricing</span>
    <div class="row" style="gap:0.5rem">
      <button class="btn-ghost btn-sm" disabled={syncing || reloading} onclick={syncLiteLlm}>
        {syncing ? "Syncing…" : "↓ Sync LiteLLM"}
      </button>
      <button class="btn-ghost btn-sm" disabled={reloading || syncing} onclick={reload}>
        {reloading ? "Reloading…" : "↺ Reload layers"}
      </button>
    </div>
  </div>
  <p class="muted small" style="margin:0.25rem 0 0.75rem">
    Layers: embedded defaults → <code>pricing.litellm.json</code> → <code>pricing.json</code> (overrides win).
    Sync pulls the LiteLLM cost map (chat/completion only); offline until you sync.
  </p>

  {#if loading && list.length === 0}
    <div class="loader"><span class="spinner"></span></div>
  {:else}
    <div class="table-wrap">
      <table class="table">
        <thead>
          <tr>
            <th>Provider</th>
            <th>Model</th>
            <th>Input/Mtok</th>
            <th>Output/Mtok</th>
            <th>Cache read</th>
            <th>Cache write</th>
            <th>Reasoning</th>
            <th>Effective from</th>
          </tr>
        </thead>
        <tbody>
          {#each list as p (p.provider_kind + p.model_id + p.effective_from)}
            <tr>
              <td><span class="badge">{p.provider_kind}</span></td>
              <td class="mono small">{p.model_id}</td>
              <td class="mono">{money(p.input_per_mtok)}</td>
              <td class="mono">{money(p.output_per_mtok)}</td>
              <td class="mono dim">{money(p.cache_read_per_mtok)}</td>
              <td class="mono dim">{money(p.cache_write_per_mtok)}</td>
              <td class="mono dim">{money(p.reasoning_per_mtok)}</td>
              <td class="dim small">{p.effective_from}</td>
            </tr>
          {:else}
            <tr><td colspan="8" class="empty">No pricing data loaded.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
