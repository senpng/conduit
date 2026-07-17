<script lang="ts">
  import { app } from "../state/app.svelte";
  import { traces as tracesApi, providers as providersApi } from "../lib/adminClient";
  import type { Provider, ReplayPlan } from "../lib/adminClient";
  import type { TraceBundle, TraceEvent } from "../lib/traceTypes";
  import { lossReportNonEmpty } from "../lib/traceTypes";
  import {
    fmtTime,
    fmtMs,
    fmtUsd,
    fmtTokens,
    sumTokens,
    providerDisplayName,
  } from "../lib/format";
  import JsonView from "../components/JsonView.svelte";

  interface Props {
    traceId: string;
  }
  let { traceId }: Props = $props();

  type Pane = "timeline" | "request" | "response" | "stream" | "meta";

  let bundle = $state<TraceBundle | null>(null);
  let providers = $state<Provider[]>([]);
  let loading = $state(true);
  let pane = $state<Pane>("timeline");
  let replayPlan = $state<ReplayPlan | null>(null);
  let replayBusy = $state(false);
  let copied = $state(false);

  async function load(id: string) {
    loading = true;
    bundle = null;
    replayPlan = null;
    try {
      const [b, plist] = await Promise.all([
        tracesApi.get(id) as Promise<TraceBundle>,
        providersApi.list().catch(() => [] as Provider[]),
      ]);
      bundle = b;
      providers = plist;
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load(traceId);
  });

  interface LossFinding {
    where: string;
    report: unknown;
  }

  /** Fixed scan per design doc: attempt_loss on routing_decided + loss_report on final_usage. */
  const lossFindings = $derived.by((): LossFinding[] => {
    if (!bundle) return [];
    const out: LossFinding[] = [];
    for (const ev of bundle.events ?? []) {
      if (ev.kind?.type === "routing_decided" && lossReportNonEmpty(ev.kind.attempt_loss)) {
        out.push({
          where: `attempt #${ev.kind.attempt_no ?? 0} (routing_decided.attempt_loss)`,
          report: ev.kind.attempt_loss,
        });
      }
      if (ev.kind?.type === "final_usage" && lossReportNonEmpty(ev.kind.loss_report)) {
        out.push({ where: "final_usage.loss_report", report: ev.kind.loss_report });
      }
    }
    return out;
  });

  function kindClass(ev: TraceEvent): string {
    switch (ev.kind?.type) {
      case "error":
        return "err";
      case "stream_delta":
        return "info";
      case "upstream_response":
        return ev.kind.status >= 400 ? "warn" : "ok";
      case "final_usage":
        return "ok";
      default:
        return "info";
    }
  }

  function kindSummary(ev: TraceEvent): string {
    const k = ev.kind;
    switch (k?.type) {
      case "request_received":
        return `alias=${k.alias}${k.stream ? " · stream" : ""}${k.wire_format ? ` · ${k.wire_format}` : ""}`;
      case "routing_decided":
        return `${providerDisplayName(providers, k.provider_id)} / ${k.model_id} · attempt #${k.attempt_no ?? 0}`;
      case "stream_delta": {
        const t = k.text_delta?.trim();
        if (t) return `#${k.seq} “${t.length > 48 ? t.slice(0, 48) + "…" : t}”`;
        const f = (k.frame ?? "").replace(/\n/g, "\\n");
        return `#${k.seq} ${f.length > 64 ? f.slice(0, 64) + "…" : f}`;
      }
      case "upstream_response": {
        const frames = k.stream_frames?.length;
        const frameNote = frames != null ? ` · ${frames} sse frames` : "";
        return `HTTP ${k.status} · ${fmtMs(k.latency_ms)}${k.ttfb_ms != null ? ` · ttfb ${fmtMs(k.ttfb_ms)}` : ""}${frameNote}`;
      }
      case "final_usage":
        return `${fmtTokens(sumTokens(k.usage ?? {}))} tok · ${fmtUsd(k.cost_usd)}`;
      case "error":
        return `${k.kind}: ${k.message}`;
      default:
        return "";
    }
  }

  async function runReplay() {
    replayBusy = true;
    try {
      replayPlan = await tracesApi.replay(traceId, true);
      if (replayPlan.billed || replayPlan.upstream_called) {
        app.toast("unexpected: dry-run reported billing or upstream call", "error");
      }
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      replayBusy = false;
    }
  }

  async function copyId() {
    try {
      await navigator.clipboard.writeText(traceId);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      app.toast("Clipboard write failed", "warn");
    }
  }

  const PANES = $derived.by((): { id: Pane; label: string; key: string }[] => {
    const base: { id: Pane; label: string; key: string }[] = [
      { id: "timeline", label: "Timeline", key: "1" },
      { id: "request", label: "Request (wire)", key: "2" },
      { id: "response", label: "Response", key: "3" },
    ];
    if (bundle?.stream_frames && bundle.stream_frames.length > 0) {
      base.push({ id: "stream", label: "SSE frames", key: "4" });
      base.push({ id: "meta", label: "Meta", key: "5" });
    } else {
      base.push({ id: "meta", label: "Meta", key: "4" });
    }
    return base;
  });

  function onKeydown(e: KeyboardEvent) {
    if (app.modalActive) return;
    const el = document.activeElement as HTMLElement | null;
    if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT")) {
      return;
    }
    const hit = PANES.find((p) => p.key === e.key);
    if (hit) {
      e.preventDefault();
      pane = hit.id;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="row-between">
  <div class="row-gap">
    <span class="mono small dim">{traceId}</span>
    <button class="btn-icon" title="Copy trace id" onclick={copyId}>
      {copied ? "✓" : "⧉"}
    </button>
  </div>
  <button class="btn-ghost btn-sm" disabled={replayBusy} onclick={runReplay}>
    {replayBusy ? "Replaying…" : "⟳ Dry-run replay"}
  </button>
</div>

{#if loading}
  <div class="loader"><span class="spinner"></span></div>
{:else if !bundle}
  <p class="empty">Trace not found (or pruned from index).</p>
{:else}
  <div class="pane-tabs">
    {#each PANES as p}
      <button class="pane-tab" class:active={pane === p.id} onclick={() => (pane = p.id)}>
        {p.label}<span class="kbd">{p.key}</span>
      </button>
    {/each}
  </div>

  {#if pane === "timeline"}
    <div class="timeline">
      {#each bundle.events ?? [] as ev (ev.id)}
        <div class="timeline-item">
          <span class="timeline-kind {kindClass(ev)}">{ev.kind?.type ?? "?"}</span>
          <span class="timeline-detail">{kindSummary(ev)}</span>
          <span class="muted tiny mono" style="margin-left:auto; flex-shrink:0">
            {fmtTime(ev.ts)}
          </span>
        </div>
      {:else}
        <p class="empty">No events in bundle.</p>
      {/each}
    </div>
  {:else if pane === "request"}
    <div style="display:flex; flex-direction:column; gap:10px">
      {#if bundle.wire_format}
        <span class="dim small mono">wire_format: {bundle.wire_format}</span>
      {/if}
      <div class="small dim">Request headers</div>
      <JsonView data={bundle.request_headers ?? null} />
      <div class="small dim">Client wire body</div>
      <JsonView data={bundle.request ?? null} />
      {#if bundle.request_ir}
        <div class="small dim">Canonical IR</div>
        <JsonView data={bundle.request_ir} />
      {/if}
    </div>
  {:else if pane === "response"}
    <div style="display:flex; flex-direction:column; gap:10px">
      {#if bundle.wire_format}
        <span class="dim small mono">
          wire_format: {bundle.wire_format}
          {#if bundle.stream} · stream{/if}
        </span>
      {/if}
      <div class="small dim">Response headers</div>
      <JsonView data={bundle.response_headers ?? null} />
      <div class="small dim">Response body</div>
      <JsonView data={bundle.response ?? null} />
    </div>
  {:else if pane === "stream"}
    <pre class="mono small" style="white-space:pre-wrap; margin:0; padding:12px; background:var(--bg-elevated); border-radius:8px; max-height:70vh; overflow:auto">{(bundle.stream_frames ?? []).join("")}</pre>
  {:else}
    <div style="display:flex; flex-direction:column; gap:14px">
      <div>
        <span class="panel-title">Codec loss</span>
        {#if lossFindings.length === 0}
          <p class="dim small" style="margin-top:6px">LossReport: (none)</p>
        {:else}
          {#each lossFindings as f, i (i)}
            <div style="margin-top:8px">
              <div class="small" style="color:var(--amber)">⚠ {f.where}</div>
              <JsonView data={f.report} />
            </div>
          {/each}
        {/if}
      </div>

      {#if replayPlan}
        <div>
          <span class="panel-title">Replay plan (dry-run)</span>
          <div class="mono small" style="margin:6px 0; color:var(--text-dim)">
            upstream_called: {String(replayPlan.upstream_called ?? false)} · billed:
            {String(replayPlan.billed ?? false)}
          </div>
          {#if replayPlan.routing_error}
            <div class="form-error">routing_error: {replayPlan.routing_error}</div>
          {/if}
          <div class="small dim" style="margin:8px 0 4px">intended_target</div>
          <JsonView data={replayPlan.intended_target ?? null} />
          <div class="small dim" style="margin:8px 0 4px">request_summary</div>
          <JsonView data={replayPlan.request_summary ?? null} />
        </div>
      {/if}

      <div>
        <span class="panel-title">Raw bundle</span>
        <details style="margin-top:6px">
          <summary class="dim small" style="cursor:pointer">
            {bundle.events?.length ?? 0} events (raw JSON)
          </summary>
          <JsonView data={bundle.events ?? []} />
        </details>
      </div>
    </div>
  {/if}
{/if}
