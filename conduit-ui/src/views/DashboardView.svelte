<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import {
    providers as providersApi,
    routes as routesApi,
    keys as keysApi,
    usage as usageApi,
  } from "../lib/consoleClient";
  import type { UsageRecord } from "../lib/consoleClient";
  import { fmtUsd2, fmtMs, fmtAgo, shortId } from "../lib/format";
  import Alert from "../components/app/Alert.svelte";
  import StatCard from "../components/app/StatCard.svelte";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import Spinner from "../components/app/Spinner.svelte";
  import EmptyState from "../components/app/EmptyState.svelte";
  import Button from "../components/ui/button.svelte";
  import {
    tableWrapClass,
    tableClass,
    thClass,
    tdClass,
    monoClass,
    dimClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";

  let providerCount = $state<number | null>(null);
  let routeCount = $state<number | null>(null);
  let keyCount = $state<number | null>(null);
  let monthSpend = $state<number | null>(null);
  let usagePeriod = $state("");
  let recent = $state<UsageRecord[]>([]);
  let loading = $state(true);

  async function load() {
    loading = true;
    try {
      const [p, r, k, b, u] = await Promise.all([
        providersApi.list(),
        routesApi.list(),
        keysApi.list(),
        usageApi.summary().catch(() => null),
        usageApi.list(5).catch(() => ({ entries: [] as UsageRecord[] })),
      ]);
      providerCount = p.length;
      routeCount = r.length;
      keyCount = k.length;
      if (b) {
        usagePeriod = b.period;
        monthSpend = b.total_usd ?? 0;
      }
      recent = u.entries ?? [];
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("dashboard", () => void load());
  });
  onDestroy(() => unregister?.());
</script>

{#if !app.isLoopback}
  <Alert variant="warning" class="mb-4">
    Console endpoint is not loopback ({app.consoleBase}) — OAuth PKCE callbacks bind to
    the daemon machine; prefer device-code or API-key providers remotely.
  </Alert>
{/if}

{#if app.healthError}
  <Alert variant="danger" class="mb-4">
    Daemon offline — start <span class="font-mono">conduitd</span> and ensure console API is
    reachable at {app.consoleBase}.
  </Alert>
{/if}

<div class="mb-4 grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5">
  <StatCard
    label="Daemon"
    value={app.healthError ? "offline" : "online"}
    valueClass={app.healthError ? "text-[var(--danger)]" : "text-[var(--success)]"}
    sub={app.health ? `v${app.health.version} · ${fmtMs(app.rttMs)}` : "unreachable"}
  />
  <StatCard label="Providers" value={providerCount ?? "—"} sub="upstream accounts" />
  <StatCard label="Routes" value={routeCount ?? "—"} sub="model aliases" />
  <StatCard label="Keys" value={keyCount ?? "—"} sub="downstream credentials" />
  <StatCard
    label={usagePeriod ? `Spend (${usagePeriod})` : "Spend"}
    value={fmtUsd2(monthSpend)}
    sub="current UTC month"
  />
</div>

<div class="mb-4 grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
  <Button variant="outline" onclick={() => app.goto("providers")}>+ Provider</Button>
  <Button variant="outline" onclick={() => app.goto("routes")}>+ Route</Button>
  <Button variant="outline" onclick={() => app.goto("usage")}>Open Usage</Button>
</div>

<Card>
  <PageHeader title="Recent usage">
    {#snippet actions()}
      <Button variant="ghost" size="sm" onclick={() => app.goto("usage")}>View all →</Button>
    {/snippet}
  </PageHeader>
  {#if loading && recent.length === 0}
    <Spinner />
  {:else if recent.length === 0}
    <EmptyState title="No usage recorded yet." />
  {:else}
    <div class={tableWrapClass}>
      <table class={tableClass}>
        <thead>
          <tr>
            <th class={thClass}>Time</th>
            <th class={thClass}>Alias</th>
            <th class={thClass}>Model</th>
            <th class={thClass}>Cost</th>
            <th class={thClass}>Request</th>
          </tr>
        </thead>
        <tbody>
          {#each recent as r (r.id)}
            <tr>
              <td class={cn(tdClass, dimClass, "text-xs")} title={r.ts ?? ""}>
                {fmtAgo(r.ts)}
              </td>
              <td class={cn(tdClass, monoClass)}>{r.alias || "—"}</td>
              <td class={cn(tdClass, monoClass, dimClass)}>{r.model_id || "—"}</td>
              <td class={cn(tdClass, monoClass, dimClass)}>{fmtUsd2(r.cost_usd)}</td>
              <td class={cn(tdClass, monoClass, dimClass, "text-xs")}>
                {shortId(r.request_id, 10)}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</Card>
