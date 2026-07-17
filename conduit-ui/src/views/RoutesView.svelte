<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { routes as routesApi, providers as providersApi } from "../lib/consoleClient";
  import type { Provider, Route } from "../lib/consoleClient";
  import { fmtDate, providerDisplayName } from "../lib/format";
  import JsonView from "../components/JsonView.svelte";
  import RouteWizard from "./RouteWizard.svelte";

  let list = $state<Route[]>([]);
  let providers = $state<Provider[]>([]);
  let loading = $state(true);
  let wizardFor = $state<Route | "new" | null>(null);
  let expandedId = $state<string | null>(null);

  async function load() {
    loading = true;
    try {
      const [routes, plist] = await Promise.all([
        routesApi.list(),
        providersApi.list().catch(() => [] as Provider[]),
      ]);
      list = routes;
      providers = plist;
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("routes", () => void load());
  });
  onDestroy(() => unregister?.());

  function targetCount(r: Route): number | "?" {
    try {
      const t = JSON.parse(r.targets_json);
      return Array.isArray(t) ? t.length : "?";
    } catch {
      return "?";
    }
  }

  function parsedTargets(r: Route): unknown {
    try {
      return JSON.parse(r.targets_json);
    } catch {
      return r.targets_json;
    }
  }

  /** Targets with provider_id resolved to display name for the expand panel. */
  function displayTargets(r: Route): unknown {
    const raw = parsedTargets(r);
    if (!Array.isArray(raw)) return raw;
    return raw.map((t: Record<string, unknown>) => {
      const id = typeof t.provider_id === "string" ? t.provider_id : "";
      const name = providerDisplayName(providers, id);
      const { provider_id: _drop, ...rest } = t;
      return {
        provider: name,
        ...rest,
      };
    });
  }

  async function toggleEnabled(r: Route) {
    try {
      await routesApi.update(r.id, { enabled: !r.enabled });
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function remove(r: Route) {
    const ok = await app.askConfirm({
      title: `Delete route "${r.match_alias}"?`,
      body: `id ${r.id} · strategy ${r.strategy}. Downstream calls for this alias will fail routing. This cannot be undone.`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await routesApi.delete(r.id);
      app.toast(`Route "${r.match_alias}" deleted`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }
</script>

<div class="panel">
  <div class="row-between">
    <span class="panel-title">Model alias routes</span>
    <button class="btn-primary" onclick={() => (wizardFor = "new")}>＋ Add route</button>
  </div>

  {#if loading && list.length === 0}
    <div class="loader"><span class="spinner"></span></div>
  {:else}
    <div class="table-wrap">
      <table class="table">
        <thead>
          <tr>
            <th>Alias</th>
            <th>Strategy</th>
            <th>Targets</th>
            <th>Status</th>
            <th>Created</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each list as r (r.id)}
            <tr
              class="clickable"
              onclick={() => (expandedId = expandedId === r.id ? null : r.id)}
            >
              <td class="mono">{r.match_alias}</td>
              <td><span class="badge accent">{r.strategy}</span></td>
              <td class="mono dim">{targetCount(r)}</td>
              <td>
                <button
                  class="pill"
                  class:on={r.enabled}
                  class:off={!r.enabled}
                  style="border:none; cursor:pointer"
                  title="Toggle enabled"
                  onclick={(e) => {
                    e.stopPropagation();
                    void toggleEnabled(r);
                  }}
                >
                  {r.enabled ? "enabled" : "disabled"}
                </button>
              </td>
              <td class="dim small">{fmtDate(r.created_at)}</td>
              <td class="actions">
                <button
                  class="btn-icon"
                  title="Edit"
                  onclick={(e) => {
                    e.stopPropagation();
                    wizardFor = r;
                  }}>✎</button
                >
                <button
                  class="btn-icon danger"
                  title="Delete"
                  onclick={(e) => {
                    e.stopPropagation();
                    void remove(r);
                  }}>✕</button
                >
              </td>
            </tr>
            {#if expandedId === r.id}
              <tr>
                <td colspan="6" style="background:var(--surface-2)">
                  <div class="small dim" style="margin-bottom:6px">
                    targets · route <span class="mono">{r.match_alias}</span>
                  </div>
                  <JsonView data={displayTargets(r)} />
                </td>
              </tr>
            {/if}
          {:else}
            <tr><td colspan="6" class="empty">No routes configured.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

{#if wizardFor === "new"}
  <RouteWizard
    ondone={(changed) => {
      wizardFor = null;
      if (changed) void load();
    }}
  />
{:else if wizardFor}
  <RouteWizard
    existing={wizardFor}
    ondone={(changed) => {
      wizardFor = null;
      if (changed) void load();
    }}
  />
{/if}
