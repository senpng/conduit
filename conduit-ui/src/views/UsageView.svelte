<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import {
    usage as usageApi,
    keys as keysApi,
    providers as providersApi,
  } from "../lib/adminClient";
  import type {
    UsageSummaryResponse,
    UsageRecord,
    UsageSummaryEntry,
  } from "../lib/adminClient";
  import {
    fmtUsd,
    fmtUsd2,
    fmtDay,
    fmtAgo,
    fmtTokens,
    dayKey,
    shortId,
    providerNameMap,
  } from "../lib/format";

  let summary = $state<UsageSummaryResponse | null>(null);
  let records = $state<UsageRecord[]>([]);
  let keyNames = $state<Map<string, string>>(new Map());
  let providerNames = $state<Map<string, string>>(new Map());
  let keyFilter = $state("");
  let limit = $state(500);
  let initialLoading = $state(true);
  let hoveredDay = $state<string | null>(null);

  async function loadNames(): Promise<void> {
    try {
      const [ks, ps] = await Promise.all([keysApi.list(), providersApi.list()]);
      keyNames = new Map(ks.map((k) => [k.id, k.name]));
      providerNames = providerNameMap(ps);
    } catch {
      /* names fall back to short ids */
    }
  }

  async function load(): Promise<void> {
    try {
      const [s, r] = await Promise.all([
        usageApi.summary(),
        usageApi.list(limit, keyFilter || undefined),
      ]);
      summary = s;
      records = r.entries ?? [];
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      initialLoading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void loadNames();
    void load();
    unregister = app.registerRefresher("usage", () => void load());
  });
  onDestroy(() => unregister?.());

  // ── Totals (summary is period-accurate; cards respect the key filter) ────

  const summaryEntries = $derived(summary?.entries ?? []);
  const filteredEntries = $derived(
    keyFilter
      ? summaryEntries.filter((e) => e.downstream_key_id === keyFilter)
      : summaryEntries,
  );
  const totals = $derived.by(() => {
    const usd = filteredEntries.reduce((s, e) => s + e.total_usd, 0);
    const req = filteredEntries.reduce((s, e) => s + e.request_count, 0);
    const tok = filteredEntries.reduce(
      (s, e) => s + (e.prompt_tokens ?? 0) + (e.completion_tokens ?? 0),
      0,
    );
    return { usd, req, tok, avg: req > 0 ? usd / req : 0 };
  });

  function keyLabel(id: string | null): string {
    if (!id) return "(anonymous)";
    return keyNames.get(id) ?? shortId(id, 8);
  }

  // ── By day (from loaded records — window labeled honestly) ───────────────

  interface DayRow {
    day: string;
    cost: number;
    requests: number;
    tokens: number;
  }

  const byDay = $derived.by((): DayRow[] => {
    const m = new Map<string, DayRow>();
    for (const r of records) {
      const k = dayKey(r.ts);
      let d = m.get(k);
      if (!d) {
        d = { day: k, cost: 0, requests: 0, tokens: 0 };
        m.set(k, d);
      }
      d.cost += r.cost_usd;
      d.requests += 1;
      d.tokens += r.total_tokens ?? 0;
    }
    return [...m.values()].sort((a, b) => (a.day < b.day ? -1 : 1));
  });

  const dayMax = $derived(Math.max(1e-9, ...byDay.map((d) => d.cost)));
  const peakDay = $derived(
    byDay.reduce((best, d) => (d.cost > (best?.cost ?? -1) ? d : best), null as DayRow | null),
  );
  const dayWindow = $derived(
    byDay.length > 0
      ? `${fmtDay(byDay[0].day)} → ${fmtDay(byDay[byDay.length - 1].day)}`
      : "",
  );

  /** Sparse x labels: at most ~7 ticks. */
  function showDayLabel(i: number, n: number): boolean {
    if (n <= 8) return true;
    const step = Math.ceil(n / 7);
    return i % step === 0;
  }

  // ── By model / alias (from loaded records) ───────────────────────────────

  interface ModelRow {
    label: string;
    kind: string | null;
    requests: number;
    tokens: number;
    cost: number;
  }

  const byModel = $derived.by((): ModelRow[] => {
    const m = new Map<string, ModelRow>();
    for (const r of records) {
      const label = r.alias || r.model_id || "(unknown)";
      let row = m.get(label);
      if (!row) {
        row = { label, kind: r.provider_kind, requests: 0, tokens: 0, cost: 0 };
        m.set(label, row);
      }
      row.requests += 1;
      row.tokens += r.total_tokens ?? 0;
      row.cost += r.cost_usd;
    }
    const all = [...m.values()].sort((a, b) => b.cost - a.cost);
    if (all.length <= 10) return all;
    const top = all.slice(0, 9);
    const rest = all.slice(9);
    top.push({
      label: `Other (${rest.length})`,
      kind: null,
      requests: rest.reduce((s, x) => s + x.requests, 0),
      tokens: rest.reduce((s, x) => s + x.tokens, 0),
      cost: rest.reduce((s, x) => s + x.cost, 0),
    });
    return top;
  });

  const recordsCostTotal = $derived(records.reduce((s, r) => s + r.cost_usd, 0));

  function share(cost: number, total: number): number {
    return total > 0 ? Math.round((cost / total) * 1000) / 10 : 0;
  }

  function selectKey(id: string): void {
    keyFilter = keyFilter === id ? "" : id;
    void load();
  }

  function tokenTitle(r: UsageRecord): string {
    const parts = [`in ${r.prompt_tokens}`, `out ${r.completion_tokens}`];
    if (r.reasoning_tokens) parts.push(`reasoning ${r.reasoning_tokens}`);
    if (r.cache_read_tokens) parts.push(`cache-read ${r.cache_read_tokens}`);
    if (r.cache_write_tokens) parts.push(`cache-write ${r.cache_write_tokens}`);
    return parts.join(" · ");
  }
</script>

{#if initialLoading}
  <div class="loader"><span class="spinner"></span></div>
{:else}
  <!-- Filter row scopes everything below. -->
  <div class="panel">
    <div class="row-between">
      <div class="row-gap">
        <label style="flex-direction:row; align-items:center; gap:8px">
          <span class="dim small">Key</span>
          <select style="width:auto" bind:value={keyFilter} onchange={() => void load()}>
            <option value="">All keys</option>
            {#each summaryEntries as e (e.downstream_key_id)}
              <option value={e.downstream_key_id}>{keyLabel(e.downstream_key_id)}</option>
            {/each}
          </select>
        </label>
        <label style="flex-direction:row; align-items:center; gap:8px">
          <span class="dim small">Records</span>
          <select
            style="width:auto"
            bind:value={limit}
            onchange={() => void load()}
          >
            <option value={100}>100</option>
            <option value={500}>500</option>
            <option value={1000}>1000</option>
          </select>
        </label>
        {#if keyFilter}
          <button class="btn-ghost btn-sm" onclick={() => selectKey("")}>
            ✕ {keyLabel(keyFilter)}
          </button>
        {/if}
      </div>
      <span class="dim small mono">{summary?.period ?? ""} (UTC)</span>
    </div>
  </div>

  <!-- Stat cards: period-accurate from summary -->
  <div class="card-grid">
    <div class="stat-card">
      <span class="stat-label">Spend</span>
      <span class="stat-value">{fmtUsd2(totals.usd)}</span>
      <span class="stat-sub">{keyFilter ? keyLabel(keyFilter) : "all keys"} · current month</span>
    </div>
    <div class="stat-card">
      <span class="stat-label">Requests</span>
      <span class="stat-value">{totals.req.toLocaleString()}</span>
      <span class="stat-sub">completed with usage</span>
    </div>
    <div class="stat-card">
      <span class="stat-label">Tokens</span>
      <span class="stat-value">{fmtTokens(totals.tok)}</span>
      <span class="stat-sub">prompt + completion</span>
    </div>
    <div class="stat-card">
      <span class="stat-label">Avg cost / request</span>
      <span class="stat-value">{fmtUsd(totals.avg)}</span>
      <span class="stat-sub">this period</span>
    </div>
  </div>

  <!-- Daily spend chart (from loaded records) -->
  <div class="panel">
    <div class="row-between">
      <span class="panel-title">Daily spend</span>
      <span class="muted tiny">
        {dayWindow} · from latest {records.length} loaded records
      </span>
    </div>
    {#if byDay.length === 0}
      <p class="empty">No usage records in window.</p>
    {:else}
      <div class="chart" role="img" aria-label="Daily spend bar chart">
        {#each byDay as d (d.day)}
          <!-- Columns are real buttons: keyboard focus/Enter pins the same
               tooltip hover shows; values also in the Daily table below. -->
          <button
            type="button"
            class="chart-col"
            aria-label="{fmtDay(d.day)}: {fmtUsd(d.cost)} across {d.requests} requests"
            onmouseenter={() => (hoveredDay = d.day)}
            onmouseleave={() => (hoveredDay = null)}
            onfocus={() => (hoveredDay = d.day)}
            onblur={() => (hoveredDay = null)}
            onclick={() => (hoveredDay = hoveredDay === d.day ? null : d.day)}
          >
            {#if d === peakDay}
              <span class="chart-peak">{fmtUsd2(d.cost)}</span>
            {/if}
            {#if hoveredDay === d.day}
              <div class="chart-tip">
                <strong>{fmtUsd(d.cost)}</strong>
                <span>{fmtDay(d.day)} · {d.requests} reqs · {fmtTokens(d.tokens)} tok</span>
              </div>
            {/if}
            <div
              class="chart-bar"
              style:height="{Math.max(2, (d.cost / dayMax) * 100)}%"
            ></div>
          </button>
        {/each}
      </div>
      <div class="chart-x">
        {#each byDay as d, i (d.day)}
          <span>{showDayLabel(i, byDay.length) ? fmtDay(d.day) : ""}</span>
        {/each}
      </div>
      <details>
        <summary class="dim small" style="cursor:pointer">Daily table</summary>
        <table class="table" style="margin-top:8px">
          <thead>
            <tr><th>Day</th><th style="text-align:right">Requests</th><th style="text-align:right">Tokens</th><th style="text-align:right">Spend</th></tr>
          </thead>
          <tbody>
            {#each [...byDay].reverse() as d (d.day)}
              <tr>
                <td class="mono small">{fmtDay(d.day)}</td>
                <td class="mono" style="text-align:right">{d.requests}</td>
                <td class="mono" style="text-align:right">{fmtTokens(d.tokens)}</td>
                <td class="mono" style="text-align:right">{fmtUsd(d.cost)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}
  </div>

  <div class="split">
    <!-- By downstream key (period-accurate summary) -->
    <div class="panel">
      <span class="panel-title">By key — this period</span>
      {#if filteredEntries.length === 0}
        <p class="empty">No usage this period.</p>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th>Key</th>
              <th style="text-align:right">Reqs</th>
              <th style="text-align:right">Tokens</th>
              <th style="text-align:right">Spend</th>
              <th style="width:26%">Share</th>
            </tr>
          </thead>
          <tbody>
            {#each filteredEntries as e (e.downstream_key_id)}
              <tr
                class="clickable"
                class:selected={keyFilter === e.downstream_key_id}
                title="Filter ledger to this key"
                onclick={() => selectKey(e.downstream_key_id)}
              >
                <td>
                  <div>{keyLabel(e.downstream_key_id)}</div>
                  <div class="muted tiny mono">{shortId(e.downstream_key_id, 8)}</div>
                </td>
                <td class="mono" style="text-align:right">{e.request_count}</td>
                <td class="mono dim" style="text-align:right">
                  {fmtTokens((e.prompt_tokens ?? 0) + (e.completion_tokens ?? 0))}
                </td>
                <td class="mono" style="text-align:right">{fmtUsd(e.total_usd)}</td>
                <td>
                  <div class="row-gap" style="flex-wrap:nowrap; gap:6px">
                    <div class="meter" style="flex:1">
                      <div
                        class="meter-fill"
                        style:width="{share(e.total_usd, totals.usd)}%"
                      ></div>
                    </div>
                    <span class="muted tiny mono" style="width:38px; text-align:right">
                      {share(e.total_usd, totals.usd)}%
                    </span>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>

    <!-- By model / alias (from loaded records) -->
    <div class="panel">
      <span class="panel-title">By model / alias — loaded window</span>
      {#if byModel.length === 0}
        <p class="empty">No records in window.</p>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th>Alias</th>
              <th style="text-align:right">Reqs</th>
              <th style="text-align:right">Tokens</th>
              <th style="text-align:right">Cost</th>
              <th style="width:26%">Share</th>
            </tr>
          </thead>
          <tbody>
            {#each byModel as m (m.label)}
              <tr>
                <td>
                  <span class="mono small">{m.label}</span>
                  {#if m.kind}<span class="badge" style="margin-left:6px">{m.kind}</span>{/if}
                </td>
                <td class="mono" style="text-align:right">{m.requests}</td>
                <td class="mono dim" style="text-align:right">{fmtTokens(m.tokens)}</td>
                <td class="mono" style="text-align:right">{fmtUsd(m.cost)}</td>
                <td>
                  <div class="row-gap" style="flex-wrap:nowrap; gap:6px">
                    <div class="meter" style="flex:1">
                      <div
                        class="meter-fill"
                        style:width="{share(m.cost, recordsCostTotal)}%"
                      ></div>
                    </div>
                    <span class="muted tiny mono" style="width:38px; text-align:right">
                      {share(m.cost, recordsCostTotal)}%
                    </span>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  </div>

  <!-- Per-request ledger -->
  <div class="panel">
    <div class="row-between">
      <span class="panel-title">Request ledger</span>
      <span class="muted tiny">
        latest {records.length} records{keyFilter ? ` · ${keyLabel(keyFilter)}` : ""} ·
        click a row for the audit trail
      </span>
    </div>
    {#if records.length === 0}
      <p class="empty">No usage records yet.</p>
    {:else}
      <div class="table-wrap" style="max-height:480px; overflow-y:auto">
        <table class="table">
          <thead>
            <tr>
              <th>Time</th>
              <th>Alias</th>
              <th>Provider / Model</th>
              <th>Key</th>
              <th style="text-align:right">Tokens</th>
              <th style="text-align:right">Cost</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each records as r (r.id)}
              <tr
                class="clickable"
                title="Open audit trail {shortId(r.request_id, 10)}"
                onclick={() => app.openTrace(r.request_id)}
              >
                <td class="dim small" style="white-space:nowrap" title={r.ts}>{fmtAgo(r.ts)}</td>
                <td class="mono">{r.alias ?? "—"}</td>
                <td class="dim small">
                  {#if r.provider_id}
                    <span title={r.provider_id}>{providerNames.get(r.provider_id) ?? r.provider_id}</span>
                    {#if r.model_id}<span class="muted mono"> / {r.model_id}</span>{/if}
                  {:else}
                    —
                  {/if}
                </td>
                <td class="small">{keyLabel(r.downstream_key_id)}</td>
                <td class="mono dim small" style="text-align:right" title={tokenTitle(r)}>
                  {r.prompt_tokens}→{r.completion_tokens}
                </td>
                <td class="mono" style="text-align:right">{fmtUsd(r.cost_usd)}</td>
                <td style="width:1%">
                  {#if r.stream}<span class="badge" title="streamed">sse</span>{/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
{/if}
