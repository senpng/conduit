<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { live } from "../state/live.svelte";
  import { fmtTime, fmtMs, fmtUsd, shortId } from "../lib/format";
  import type { StatusClass } from "../lib/liveRollup";
  import StatusPill from "../components/StatusPill.svelte";

  onMount(() => live.start());
  onDestroy(() => live.stop());

  const STATUS_OPTIONS: { id: StatusClass; label: string }[] = [
    { id: "", label: "All" },
    { id: "2xx", label: "2xx" },
    { id: "4xx", label: "4xx" },
    { id: "5xx", label: "5xx" },
    { id: "err", label: "Errors" },
  ];
</script>

<div class="panel">
  <div class="row-between">
    <div class="live-statusbar">
      <span><span class="sse-dot {live.sse}"></span>{live.statusLabel}</span>
      <span class="sep">·</span>
      <span>{live.rowCount} rows</span>
      {#if live.paused}
        <span class="sep">·</span>
        <span style="color:var(--amber)">paused (+{live.pauseDrop} dropped)</span>
      {/if}
      {#if live.uiDrop > 0}
        <span class="sep">·</span>
        <span title="Frames the UI could not parse">ui_drop={live.uiDrop}</span>
      {/if}
      {#if live.laggedEventsTotal > 0}
        <span class="sep">·</span>
        <span title="Server-reported broadcast lag">
          lagged ×{live.laggedEventsTotal}
        </span>
      {/if}
      {#if live.lastEventTs}
        <span class="sep">·</span>
        <span class="muted">last {fmtTime(live.lastEventTs)}</span>
      {/if}
    </div>
    <div class="row-gap">
      <button class="btn-ghost btn-sm" class:active={live.paused} onclick={() => live.togglePause()}>
        {live.paused ? "▶ Resume" : "⏸ Pause"}
      </button>
      <button class="btn-ghost btn-sm" onclick={() => live.clear()}>Clear</button>
    </div>
  </div>

  <div class="live-filter-row">
    <input
      placeholder="Filter: alias / provider name / trace id…"
      value={live.filter.text}
      oninput={(e) => live.setFilter({ text: e.currentTarget.value })}
    />
    <div class="seg-group">
      {#each STATUS_OPTIONS as o}
        <button
          class:active={live.filter.statusClass === o.id}
          onclick={() => live.setFilter({ statusClass: o.id })}
        >
          {o.label}
        </button>
      {/each}
    </div>
    <label style="flex-direction:row; align-items:center; gap:6px; font-weight:400">
      <input
        type="checkbox"
        checked={live.filter.includeInFlight}
        onchange={(e) => live.setFilter({ includeInFlight: e.currentTarget.checked })}
      />
      <span class="dim small">in-flight</span>
    </label>
  </div>

  <div class="table-wrap" style="max-height: calc(100vh - 260px); overflow-y:auto">
    <table class="table">
      <thead>
        <tr>
          <th>Time</th>
          <th>Trace</th>
          <th>Alias</th>
          <th>Provider / Model</th>
          <th>Status</th>
          <th>Latency</th>
          <th>Cost</th>
          <th>Last</th>
        </tr>
      </thead>
      <tbody>
        {#each live.rows as row (row.traceId)}
          <tr
            class="clickable"
            onclick={() => app.openTrace(row.traceId)}
            title="Open audit detail"
          >
            <td class="dim small mono">{fmtTime(row.updatedAt)}</td>
            <td class="mono dim small">{shortId(row.traceId, 8)}</td>
            <td class="mono">{row.alias ?? "—"}</td>
            <td class="dim small">
              {#if row.providerId}
                <span title={row.providerId}>{live.providerLabel(row.providerId)}</span
                >{#if row.modelId}<span class="muted mono"> / {row.modelId}</span>{/if}
              {:else}
                —
              {/if}
            </td>
            <td>
              <StatusPill status={row.status} errorKind={row.errorKind} />
              {#if row.hasLoss}
                <span class="badge" title="Codec loss reported">loss</span>
              {/if}
            </td>
            <td class="mono dim">{fmtMs(row.latencyMs)}</td>
            <td class="mono dim">{fmtUsd(row.costUsd)}</td>
            <td class="muted tiny mono">{row.lastKind}</td>
          </tr>
        {:else}
          <tr>
            <td colspan="8" class="empty">
              {#if live.sse === "connected"}
                Waiting for gateway requests…
              {:else}
                Stream not connected — check daemon.
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>
