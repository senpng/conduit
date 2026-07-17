<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { live } from "../state/live.svelte";
  import {
    providers as providersApi,
    routes as routesApi,
    keys as keysApi,
    usage as usageApi,
    traces as tracesApi,
  } from "../lib/adminClient";
  import type { TraceIndexRow } from "../lib/adminClient";
  import { fmtUsd2, fmtMs, fmtAgo, shortId } from "../lib/format";
  import StatusPill from "../components/StatusPill.svelte";

  let providerCount = $state<number | null>(null);
  let routeCount = $state<number | null>(null);
  let keyCount = $state<number | null>(null);
  let monthSpend = $state<number | null>(null);
  let usagePeriod = $state("");
  let recent = $state<TraceIndexRow[]>([]);
  let loading = $state(true);

  async function load() {
    loading = true;
    try {
      const [p, r, k, b, t] = await Promise.all([
        providersApi.list(),
        routesApi.list(),
        keysApi.list(),
        usageApi.summary().catch(() => null),
        tracesApi.list(5).catch(() => ({ traces: [] }) as { traces: TraceIndexRow[] }),
      ]);
      providerCount = p.length;
      routeCount = r.length;
      keyCount = k.length;
      if (b) {
        usagePeriod = b.period;
        monthSpend = b.total_usd ?? 0;
      }
      recent = t.traces ?? [];
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
  <div class="warn-bar">
    ⚠ Admin base is not loopback ({app.adminBase}) — OAuth PKCE callbacks bind to
    the daemon machine; prefer device-code or API-key providers remotely.
  </div>
{/if}

<div class="card-grid">
  <div class="stat-card">
    <span class="stat-label">Daemon</span>
    <span
      class="stat-value"
      style:color={app.healthError ? "var(--red)" : "var(--green)"}
    >
      {app.healthError ? "offline" : "online"}
    </span>
    <span class="stat-sub">
      {#if app.health}v{app.health.version} · {fmtMs(app.rttMs)}{:else}unreachable{/if}
    </span>
  </div>
  <div class="stat-card">
    <span class="stat-label">Providers</span>
    <span class="stat-value">{providerCount ?? "—"}</span>
    <span class="stat-sub">upstream accounts</span>
  </div>
  <div class="stat-card">
    <span class="stat-label">Routes</span>
    <span class="stat-value">{routeCount ?? "—"}</span>
    <span class="stat-sub">model aliases</span>
  </div>
  <div class="stat-card">
    <span class="stat-label">Keys</span>
    <span class="stat-value">{keyCount ?? "—"}</span>
    <span class="stat-sub">downstream credentials</span>
  </div>
  <div class="stat-card">
    <span class="stat-label">Spend {usagePeriod ? `(${usagePeriod})` : ""}</span>
    <span class="stat-value">{fmtUsd2(monthSpend)}</span>
    <span class="stat-sub">current UTC month</span>
  </div>
  <button
    class="stat-card"
    style="cursor:pointer; text-align:left; font-family:inherit; color:inherit; border:1px solid var(--border); background:var(--surface)"
    onclick={() => app.goto("live")}
  >
    <span class="stat-label">Live stream</span>
    <span class="stat-value" style="font-size:15px">
      <span class="sse-dot {live.sse}"></span>{live.statusLabel}
    </span>
    <span class="stat-sub">{live.rowCount} rows buffered → open monitor</span>
  </button>
</div>

<div class="panel">
  <div class="row-between">
    <span class="panel-title">Recent requests</span>
    <button class="btn-ghost btn-sm" onclick={() => app.goto("traces")}>View all →</button>
  </div>
  {#if loading && recent.length === 0}
    <div class="loader"><span class="spinner"></span></div>
  {:else if recent.length === 0}
    <p class="empty">No requests recorded yet.</p>
  {:else}
    <div class="table-wrap">
      <table class="table">
        <thead>
          <tr><th>Time</th><th>Alias</th><th>Status</th><th>Latency</th><th>Trace</th></tr>
        </thead>
        <tbody>
          {#each recent as t (t.id)}
            <tr class="clickable" onclick={() => app.openTrace(t.trace_id || t.id)}>
              <td class="dim small" title={t.ts}>{fmtAgo(t.ts)}</td>
              <td class="mono">{t.alias || "—"}</td>
              <td>
                <StatusPill
                  status={t.status_code || undefined}
                  errorKind={t.error_kind ?? undefined}
                />
              </td>
              <td class="mono dim">{t.latency_ms ? fmtMs(t.latency_ms) : "—"}</td>
              <td class="mono dim small">{shortId(t.trace_id || t.id, 10)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
