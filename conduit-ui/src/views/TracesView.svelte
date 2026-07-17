<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { traces as tracesApi } from "../lib/adminClient";
  import type { TraceIndexRow } from "../lib/adminClient";
  import { fmtAgo, fmtMs, fmtUsd, shortId } from "../lib/format";
  import StatusPill from "../components/StatusPill.svelte";
  import TraceDetail from "./TraceDetail.svelte";

  let rows = $state<TraceIndexRow[]>([]);
  let loading = $state(true);
  let selectedId = $state<string | null>(null);
  let limit = $state(100);

  async function load() {
    loading = true;
    try {
      const res = await tracesApi.list(limit);
      rows = res.traces ?? [];
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("traces", () => void load());
  });
  onDestroy(() => unregister?.());

  // Deep-link from Live Monitor / Dashboard.
  $effect(() => {
    const focus = app.traceFocus;
    if (focus) {
      selectedId = focus;
      app.traceFocus = null;
    }
  });
</script>

<div class="trace-layout">
  <div class="panel">
    <div class="row-between">
      <span class="panel-title">Requests</span>
      <div class="row-gap">
        <select
          style="width:auto; padding:4px 8px"
          bind:value={limit}
          onchange={() => void load()}
        >
          <option value={50}>50</option>
          <option value={100}>100</option>
          <option value={500}>500</option>
        </select>
        <button class="btn-ghost btn-sm" onclick={() => void load()}>↺</button>
      </div>
    </div>

    {#if loading && rows.length === 0}
      <div class="loader"><span class="spinner"></span></div>
    {:else if rows.length === 0}
      <p class="empty">No traces recorded yet.</p>
    {:else}
      <div style="overflow-y:auto; max-height: calc(100vh - 220px)">
        <table class="table">
          <tbody>
            {#each rows as t (t.id)}
              {@const tid = t.trace_id || t.id}
              <tr
                class="clickable"
                class:selected={selectedId === tid}
                onclick={() => (selectedId = tid)}
              >
                <td style="width:1%">
                  <StatusPill
                    status={t.status_code || undefined}
                    errorKind={t.error_kind ?? undefined}
                  />
                </td>
                <td>
                  <div class="mono">{t.alias || "—"}</div>
                  <div class="muted tiny mono">{shortId(tid, 12)}</div>
                </td>
                <td style="text-align:right">
                  <div class="dim small mono">{t.latency_ms ? fmtMs(t.latency_ms) : "—"}</div>
                  <div class="muted tiny mono">{t.cost_usd ? fmtUsd(t.cost_usd) : ""}</div>
                </td>
                <td class="dim small" style="width:1%" title={t.ts}>{fmtAgo(t.ts)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>

  <div class="panel" style="min-height:300px">
    {#if selectedId}
      <TraceDetail traceId={selectedId} />
    {:else}
      <p class="empty">Select a request to inspect its audit trail.</p>
    {/if}
  </div>
</div>
