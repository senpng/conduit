<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { routes as routesApi, providers as providersApi } from "../lib/consoleClient";
  import type { Provider, Route } from "../lib/consoleClient";
  import { fmtDate, providerDisplayName } from "../lib/format";
  import JsonView from "../components/JsonView.svelte";
  import RouteWizard from "./RouteWizard.svelte";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import Spinner from "../components/app/Spinner.svelte";
  import Button from "../components/ui/button.svelte";
  import Badge from "../components/ui/badge.svelte";
  import {
    tableWrapClass,
    tableClass,
    thClass,
    tdClass,
    monoClass,
    dimClass,
    trClickClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";
  import { Plus, Pencil, Trash2 } from "@lucide/svelte";

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

<Card>
  <PageHeader title="Model alias routes" description="Map client model names to upstream targets.">
    {#snippet actions()}
      <Button onclick={() => (wizardFor = "new")}>
        <Plus class="h-4 w-4" />
        Add route
      </Button>
    {/snippet}
  </PageHeader>

  {#if loading && list.length === 0}
    <Spinner />
  {:else}
    <div class={tableWrapClass}>
      <table class={tableClass}>
        <thead>
          <tr>
            <th class={thClass}>Alias</th>
            <th class={thClass}>Strategy</th>
            <th class={thClass}>Targets</th>
            <th class={thClass}>Status</th>
            <th class={thClass}>Created</th>
            <th class={thClass}></th>
          </tr>
        </thead>
        <tbody>
          {#each list as r (r.id)}
            <tr
              class={trClickClass}
              onclick={() => (expandedId = expandedId === r.id ? null : r.id)}
            >
              <td class={cn(tdClass, monoClass)}>{r.match_alias}</td>
              <td class={tdClass}><Badge>{r.strategy}</Badge></td>
              <td class={cn(tdClass, monoClass, dimClass)}>{targetCount(r)}</td>
              <td class={tdClass}>
                <button
                  type="button"
                  class="cursor-pointer border-0 bg-transparent p-0"
                  title="Toggle enabled"
                  onclick={(e) => {
                    e.stopPropagation();
                    void toggleEnabled(r);
                  }}
                >
                  <Badge variant={r.enabled ? "success" : "secondary"}>
                    {r.enabled ? "enabled" : "disabled"}
                  </Badge>
                </button>
              </td>
              <td class={cn(tdClass, dimClass, "text-xs")}>{fmtDate(r.created_at)}</td>
              <td class={cn(tdClass, "w-px")}>
                <div class="flex items-center gap-0.5">
                  <Button
                    variant="ghost"
                    size="icon"
                    title="Edit"
                    onclick={(e) => {
                      e.stopPropagation();
                      wizardFor = r;
                    }}
                  >
                    <Pencil class="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    title="Delete"
                    onclick={(e) => {
                      e.stopPropagation();
                      void remove(r);
                    }}
                  >
                    <Trash2 class="h-4 w-4 text-[var(--danger)]" />
                  </Button>
                </div>
              </td>
            </tr>
            {#if expandedId === r.id}
              <tr>
                <td class={cn(tdClass, "bg-[var(--surface-muted)]")} colspan="6">
                  <div class="mb-1.5 text-xs text-[var(--text-secondary)]">
                    targets · route <span class={monoClass}>{r.match_alias}</span>
                  </div>
                  <JsonView data={displayTargets(r)} />
                </td>
              </tr>
            {/if}
          {:else}
            <tr>
              <td class={cn(tdClass, "py-8 text-center text-[var(--text-muted)]")} colspan="6">
                No routes configured.
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</Card>

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
