<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import {
    usage as usageApi,
    keys as keysApi,
    providers as providersApi,
  } from "../lib/consoleClient";
  import type {
    UsageSummaryResponse,
    UsageRecord,
  } from "../lib/consoleClient";
  import {
    fmtUsd,
    fmtUsd2,
    fmtDay,
    fmtAgo,
    fmtTokens,
    shortId,
    providerNameMap,
  } from "../lib/format";
  import PricingView from "./PricingView.svelte";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import StatCard from "../components/app/StatCard.svelte";
  import Spinner from "../components/app/Spinner.svelte";
  import EmptyState from "../components/app/EmptyState.svelte";
  import Button from "../components/ui/button.svelte";
  import Badge from "../components/ui/badge.svelte";
  import {
    selectClass,
    tableWrapClass,
    tableClass,
    thClass,
    tdClass,
    monoClass,
    dimClass,
    mutedClass,
    trClickClass,
    trSelectedClass,
    segmentGroupClass,
    segmentBtnClass,
    segmentBtnActiveClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";
  import { X } from "@lucide/svelte";

  type UsageTab = "usage" | "pricing";
  let tab = $state<UsageTab>("usage");

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
      const key = keyFilter || undefined;
      const [s, r] = await Promise.all([
        usageApi.summary(undefined, key),
        usageApi.list(limit, key),
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

  interface DayRow {
    day: string;
    cost: number;
    requests: number;
    tokens: number;
  }

  const byDay = $derived.by((): DayRow[] => {
    const rows = summary?.by_day ?? [];
    return rows.map((d) => ({
      day: d.day,
      cost: d.total_usd,
      requests: d.request_count,
      tokens: d.total_tokens ?? 0,
    }));
  });

  const dayMax = $derived(Math.max(1e-9, ...byDay.map((d) => d.cost)));
  const peakDay = $derived(
    byDay.reduce((best, d) => (d.cost > (best?.cost ?? -1) ? d : best), null as DayRow | null),
  );
  const dayWindow = $derived(
    byDay.length > 0
      ? `${fmtDay(byDay[0].day)} → ${fmtDay(byDay[byDay.length - 1].day)} · full period (UTC)`
      : "",
  );

  function showDayLabel(i: number, n: number): boolean {
    if (n <= 8) return true;
    const step = Math.ceil(n / 7);
    return i % step === 0;
  }

  interface ModelRow {
    label: string;
    kind: string | null;
    requests: number;
    tokens: number;
    cost: number;
  }

  const byModel = $derived.by((): ModelRow[] => {
    const rows = summary?.by_model ?? [];
    const all = rows.map((m) => ({
      label: m.label,
      kind: m.provider_kind,
      requests: m.request_count,
      tokens: m.total_tokens ?? 0,
      cost: m.total_usd,
    }));
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

  const modelCostTotal = $derived(byModel.reduce((s, m) => s + m.cost, 0));

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

<div class={cn(segmentGroupClass, "mb-4")}>
  <button
    type="button"
    class={cn(segmentBtnClass, "cursor-pointer border-0 bg-transparent", tab === "usage" && segmentBtnActiveClass)}
    onclick={() => (tab = "usage")}
  >
    Usage
  </button>
  <button
    type="button"
    class={cn(segmentBtnClass, "cursor-pointer border-0 bg-transparent", tab === "pricing" && segmentBtnActiveClass)}
    onclick={() => (tab = "pricing")}
  >
    Pricing
  </button>
</div>

{#if tab === "pricing"}
  <PricingView />
{:else if initialLoading}
  <Spinner />
{:else}
  <Card class="mb-4">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div class="flex flex-wrap items-center gap-3">
        <label class="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
          Key
          <select class={cn(selectClass, "w-auto")} bind:value={keyFilter} onchange={() => void load()}>
            <option value="">All keys</option>
            {#each summaryEntries as e (e.downstream_key_id)}
              <option value={e.downstream_key_id}>{keyLabel(e.downstream_key_id)}</option>
            {/each}
          </select>
        </label>
        <label class="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
          Records
          <select class={cn(selectClass, "w-auto")} bind:value={limit} onchange={() => void load()}>
            <option value={100}>100</option>
            <option value={500}>500</option>
            <option value={1000}>1000</option>
          </select>
        </label>
        {#if keyFilter}
          <Button variant="outline" size="sm" onclick={() => selectKey(keyFilter)}>
            <X class="h-3.5 w-3.5" />
            {keyLabel(keyFilter)}
          </Button>
        {/if}
      </div>
      <span class={cn(dimClass, monoClass, "text-xs")}>{summary?.period ?? ""} (UTC)</span>
    </div>
  </Card>

  <div class="mb-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
    <StatCard
      label="Spend"
      value={fmtUsd2(totals.usd)}
      sub={`${keyFilter ? keyLabel(keyFilter) : "all keys"} · current month`}
    />
    <StatCard label="Requests" value={totals.req.toLocaleString()} sub="completed with usage" />
    <StatCard label="Tokens" value={fmtTokens(totals.tok)} sub="prompt + completion" />
    <StatCard label="Avg cost / request" value={fmtUsd(totals.avg)} sub="this period" />
  </div>

  <Card class="mb-4">
    <PageHeader title="Daily spend">
      {#snippet actions()}
        <span class={cn(mutedClass, "text-[11px]")}>
          {dayWindow || "no data"}
          {#if keyFilter} · {keyLabel(keyFilter)}{/if}
        </span>
      {/snippet}
    </PageHeader>
    {#if byDay.length === 0}
      <EmptyState title="No usage this period." />
    {:else}
      <div
        class="mb-2 flex h-36 items-end gap-1"
        role="img"
        aria-label="Daily spend bar chart"
      >
        {#each byDay as d (d.day)}
          <button
            type="button"
            class="group relative flex h-full min-w-0 flex-1 flex-col items-center justify-end border-0 bg-transparent p-0"
            aria-label="{fmtDay(d.day)}: {fmtUsd(d.cost)} across {d.requests} requests"
            onmouseenter={() => (hoveredDay = d.day)}
            onmouseleave={() => (hoveredDay = null)}
            onfocus={() => (hoveredDay = d.day)}
            onblur={() => (hoveredDay = null)}
            onclick={() => (hoveredDay = hoveredDay === d.day ? null : d.day)}
          >
            {#if d === peakDay}
              <span class="mb-1 text-[10px] font-medium text-[var(--accent)]">{fmtUsd2(d.cost)}</span>
            {/if}
            {#if hoveredDay === d.day}
              <div
                class="absolute bottom-full z-10 mb-1 rounded-md border border-[var(--border)] bg-[var(--surface)] px-2 py-1 text-left text-[11px] shadow-sm"
              >
                <strong class="text-[var(--text)]">{fmtUsd(d.cost)}</strong>
                <div class="text-[var(--text-muted)]">
                  {fmtDay(d.day)} · {d.requests} reqs · {fmtTokens(d.tokens)} tok
                </div>
              </div>
            {/if}
            <div
              class="w-full max-w-[28px] rounded-t-sm bg-[var(--accent)]/80 transition-colors group-hover:bg-[var(--accent)]"
              style:height="{Math.max(2, (d.cost / dayMax) * 100)}%"
            ></div>
          </button>
        {/each}
      </div>
      <div class="mb-3 flex gap-1 text-[10px] text-[var(--text-muted)]">
        {#each byDay as d, i (d.day)}
          <span class="min-w-0 flex-1 text-center truncate">
            {showDayLabel(i, byDay.length) ? fmtDay(d.day) : ""}
          </span>
        {/each}
      </div>
      <details>
        <summary class={cn("cursor-pointer text-xs", dimClass)}>Daily table</summary>
        <div class={cn(tableWrapClass, "mt-2")}>
          <table class={tableClass}>
            <thead>
              <tr>
                <th class={thClass}>Day</th>
                <th class={cn(thClass, "text-right")}>Requests</th>
                <th class={cn(thClass, "text-right")}>Tokens</th>
                <th class={cn(thClass, "text-right")}>Spend</th>
              </tr>
            </thead>
            <tbody>
              {#each [...byDay].reverse() as d (d.day)}
                <tr class="hover:bg-[var(--surface-muted)]/80">
                  <td class={cn(tdClass, monoClass, "text-xs")}>{fmtDay(d.day)}</td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{d.requests}</td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{fmtTokens(d.tokens)}</td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{fmtUsd(d.cost)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </details>
    {/if}
  </Card>

  <div class="mb-4 grid gap-4 lg:grid-cols-2">
    <Card>
      <PageHeader title="By key — this period" />
      {#if filteredEntries.length === 0}
        <EmptyState title="No usage this period." />
      {:else}
        <div class={tableWrapClass}>
          <table class={tableClass}>
            <thead>
              <tr>
                <th class={thClass}>Key</th>
                <th class={cn(thClass, "text-right")}>Reqs</th>
                <th class={cn(thClass, "text-right")}>Tokens</th>
                <th class={cn(thClass, "text-right")}>Spend</th>
                <th class={cn(thClass, "w-[26%]")}>Share</th>
              </tr>
            </thead>
            <tbody>
              {#each filteredEntries as e (e.downstream_key_id)}
                <tr
                  class={cn(trClickClass, keyFilter === e.downstream_key_id && trSelectedClass)}
                  title="Filter ledger to this key"
                  onclick={() => selectKey(e.downstream_key_id)}
                >
                  <td class={tdClass}>
                    <div>{keyLabel(e.downstream_key_id)}</div>
                    <div class={cn(mutedClass, monoClass, "text-[11px]")}>
                      {shortId(e.downstream_key_id, 8)}
                    </div>
                  </td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{e.request_count}</td>
                  <td class={cn(tdClass, monoClass, dimClass, "text-right")}>
                    {fmtTokens((e.prompt_tokens ?? 0) + (e.completion_tokens ?? 0))}
                  </td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{fmtUsd(e.total_usd)}</td>
                  <td class={tdClass}>
                    <div class="flex items-center gap-1.5">
                      <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-[var(--surface-muted)]">
                        <div
                          class="h-full rounded-full bg-[var(--accent)]"
                          style:width="{share(e.total_usd, totals.usd)}%"
                        ></div>
                      </div>
                      <span class={cn(mutedClass, monoClass, "w-9 text-right text-[11px]")}>
                        {share(e.total_usd, totals.usd)}%
                      </span>
                    </div>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </Card>

    <Card>
      <PageHeader title="By model / alias — this period" />
      {#if byModel.length === 0}
        <EmptyState title="No usage this period." />
      {:else}
        <div class={tableWrapClass}>
          <table class={tableClass}>
            <thead>
              <tr>
                <th class={thClass}>Alias</th>
                <th class={cn(thClass, "text-right")}>Reqs</th>
                <th class={cn(thClass, "text-right")}>Tokens</th>
                <th class={cn(thClass, "text-right")}>Cost</th>
                <th class={cn(thClass, "w-[26%]")}>Share</th>
              </tr>
            </thead>
            <tbody>
              {#each byModel as m (m.label)}
                <tr class="hover:bg-[var(--surface-muted)]/80">
                  <td class={tdClass}>
                    <span class={cn(monoClass, "text-xs")}>{m.label}</span>
                    {#if m.kind}
                      <Badge variant="secondary" class="ml-1.5">{m.kind}</Badge>
                    {/if}
                  </td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{m.requests}</td>
                  <td class={cn(tdClass, monoClass, dimClass, "text-right")}>{fmtTokens(m.tokens)}</td>
                  <td class={cn(tdClass, monoClass, "text-right")}>{fmtUsd(m.cost)}</td>
                  <td class={tdClass}>
                    <div class="flex items-center gap-1.5">
                      <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-[var(--surface-muted)]">
                        <div
                          class="h-full rounded-full bg-[var(--accent)]"
                          style:width="{share(m.cost, modelCostTotal)}%"
                        ></div>
                      </div>
                      <span class={cn(mutedClass, monoClass, "w-9 text-right text-[11px]")}>
                        {share(m.cost, modelCostTotal)}%
                      </span>
                    </div>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </Card>
  </div>

  <Card>
    <PageHeader title="Request ledger">
      {#snippet actions()}
        <span class={cn(mutedClass, "text-[11px]")}>
          latest {records.length} records{keyFilter ? ` · ${keyLabel(keyFilter)}` : ""}
        </span>
      {/snippet}
    </PageHeader>
    {#if records.length === 0}
      <EmptyState title="No usage records yet." />
    {:else}
      <div class={cn(tableWrapClass, "max-h-[480px]")}>
        <table class={tableClass}>
          <thead>
            <tr>
              <th class={thClass}>Time</th>
              <th class={thClass}>Alias</th>
              <th class={thClass}>Provider / Model</th>
              <th class={thClass}>Key</th>
              <th class={cn(thClass, "text-right")}>Tokens</th>
              <th class={cn(thClass, "text-right")}>Cost</th>
              <th class={thClass}></th>
            </tr>
          </thead>
          <tbody>
            {#each records as r (r.id)}
              <tr title="Request {shortId(r.request_id, 10)}">
                <td class={cn(tdClass, dimClass, "text-xs whitespace-nowrap")} title={r.ts}>
                  {fmtAgo(r.ts)}
                </td>
                <td class={cn(tdClass, monoClass)}>{r.alias ?? "—"}</td>
                <td class={cn(tdClass, dimClass, "text-xs")}>
                  {#if r.provider_id}
                    <span title={r.provider_id}>
                      {providerNames.get(r.provider_id) ?? r.provider_id}
                    </span>
                    {#if r.model_id}<span class={cn(mutedClass, monoClass)}> / {r.model_id}</span>{/if}
                  {:else}
                    —
                  {/if}
                </td>
                <td class={cn(tdClass, "text-xs")}>{keyLabel(r.downstream_key_id)}</td>
                <td
                  class={cn(tdClass, monoClass, dimClass, "text-right text-xs")}
                  title={tokenTitle(r)}
                >
                  {r.prompt_tokens}→{r.completion_tokens}
                </td>
                <td class={cn(tdClass, monoClass, "text-right")}>{fmtUsd(r.cost_usd)}</td>
                <td class={cn(tdClass, "w-px")}>
                  {#if r.stream}<Badge variant="secondary" title="streamed">sse</Badge>{/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </Card>
{/if}
