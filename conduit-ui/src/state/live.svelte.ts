/**
 * Live Monitor state: SSE lifecycle + request rollup + filter/pause/counters.
 *
 * SSE semantics (design doc R-4): `event: lagged` frames update
 * `lastLaggedSkipped`; transport errors retry with exponential backoff
 * (1s → 15s cap); `uiDrop` counts data frames the UI could not parse
 * (UI-side analog of a full event channel).
 */

import { traces as tracesApi, providers as providersApi } from "../lib/consoleClient";
import type { Provider } from "../lib/consoleClient";
import { consumeSseFrames, parseLaggedSkipped, isStubTailPayload } from "../lib/sse";
import { parseTraceEvent } from "../lib/traceTypes";
import { providerNameMap } from "../lib/format";
import {
  createRollup,
  applyEvent,
  seedFromIndex,
  clearRollup,
  visibleRows,
  EMPTY_FILTER,
  type LiveRollup,
  type LiveFilter,
  type LiveRequestRow,
} from "../lib/liveRollup";

export type SseStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "failed";

const BACKOFF_START_MS = 1000;
const BACKOFF_MAX_MS = 15000;
const SEED_LIMIT = 250;

class LiveState {
  /** Rollup data; mutated in place, `version` signals change. */
  private rollup: LiveRollup = createRollup();
  version = $state(0);

  sse = $state<SseStatus>("idle");
  paused = $state(false);
  filter = $state<LiveFilter>({ ...EMPTY_FILTER });

  lastLaggedSkipped = $state<number | null>(null);
  laggedEventsTotal = $state(0);
  /** Unparseable data frames (UI-side drop analog). */
  uiDrop = $state(0);
  pauseDrop = $state(0);
  lastEventTs = $state<string | null>(null);
  totalCount = $state(0);
  /** Cached providers for id → name display (and filter by name). */
  providers = $state<Provider[]>([]);

  private ac: AbortController | null = null;
  private retryMs = BACKOFF_START_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private running = false;
  private generation = 0;

  get rows(): LiveRequestRow[] {
    void this.version; // dependency
    void this.providers;
    void this.filter;
    const names = providerNameMap(this.providers);
    return visibleRows(this.rollup, this.filter, (id) => names.get(id));
  }

  /** Resolve provider id to display name (falls back to id). */
  providerLabel(id: string | undefined | null): string {
    if (!id) return "—";
    const names = providerNameMap(this.providers);
    return names.get(id) ?? id;
  }

  get rowCount(): number {
    void this.version;
    return this.rollup.order.length;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    const generation = ++this.generation;
    void this.seedThenStream(generation);
  }

  stop(): void {
    this.generation++;
    this.running = false;
    this.ac?.abort();
    this.ac = null;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.sse = "idle";
  }

  togglePause(): void {
    this.paused = !this.paused;
  }

  clear(): void {
    clearRollup(this.rollup);
    this.version++;
  }

  setFilter(patch: Partial<LiveFilter>): void {
    this.filter = { ...this.filter, ...patch };
  }

  private bump(): void {
    this.version++;
  }

  private isCurrent(generation: number): boolean {
    return this.running && this.generation === generation;
  }

  private async refreshProviders(generation: number): Promise<void> {
    try {
      const providers = await providersApi.list();
      if (this.isCurrent(generation)) this.providers = providers;
    } catch {
      /* keep last good cache; live still works with raw ids */
    }
  }

  private async seedThenStream(generation: number): Promise<void> {
    void this.refreshProviders(generation);
    try {
      const res = await tracesApi.list(SEED_LIMIT, true);
      if (!this.isCurrent(generation)) return;
      seedFromIndex(this.rollup, res.traces ?? []);
      this.bump();
    } catch {
      // Seed failure is non-fatal: the stream still delivers new events.
    }
    while (this.isCurrent(generation)) {
      await this.streamOnce(generation);
      if (!this.isCurrent(generation)) break;
      this.sse = "reconnecting";
      await new Promise((resolve) => {
        this.retryTimer = setTimeout(resolve, this.retryMs);
      });
      if (!this.isCurrent(generation)) break;
      this.retryMs = Math.min(this.retryMs * 2, BACKOFF_MAX_MS);
    }
  }

  private async streamOnce(generation: number): Promise<void> {
    if (!this.isCurrent(generation)) return;
    this.sse = this.sse === "reconnecting" ? "reconnecting" : "connecting";
    const ac = new AbortController();
    this.ac = ac;
    try {
      const res = await fetch(tracesApi.streamUrl(), {
        headers: { Accept: "text/event-stream" },
        signal: ac.signal,
      });
      if (!res.ok || !res.body) {
        throw new Error(`stream HTTP ${res.status}`);
      }
      if (!this.isCurrent(generation)) return;
      this.sse = "connected";
      this.retryMs = BACKOFF_START_MS;
      this.lastLaggedSkipped = null;

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      while (this.isCurrent(generation)) {
        const { done, value } = await reader.read();
        if (done) break;
        const out = consumeSseFrames(buffer, decoder.decode(value, { stream: true }));
        buffer = out.buffer;
        let dirty = false;
        for (const frame of out.frames) {
          if (frame.event === "lagged") {
            const skipped = parseLaggedSkipped(frame.data);
            this.lastLaggedSkipped = skipped;
            this.laggedEventsTotal += 1;
            continue;
          }
          if (isStubTailPayload(frame.data)) {
            throw new Error("console stream returned stub payload");
          }
          if (this.paused) {
            this.pauseDrop += 1;
            continue;
          }
          const ev = parseTraceEvent(frame.data);
          if (!ev) {
            this.uiDrop += 1;
            continue;
          }
          applyEvent(this.rollup, ev);
          this.lastEventTs = ev.ts ?? null;
          this.totalCount += 1;
          dirty = true;
        }
        if (dirty) this.bump();
      }
    } catch (e: unknown) {
      if ((e as { name?: string })?.name === "AbortError") return;
      if (this.isCurrent(generation)) this.sse = "reconnecting";
    } finally {
      if (this.ac === ac) this.ac = null;
    }
    if (this.isCurrent(generation) && this.sse !== "reconnecting") {
      // Clean EOF from server: treat as reconnect.
      this.sse = "reconnecting";
    }
  }

  /** Permanent failure surfaced to the view (e.g. after user stops). */
  get statusLabel(): string {
    switch (this.sse) {
      case "connected":
        return this.lastLaggedSkipped != null
          ? `SSE: lagged skipped=${this.lastLaggedSkipped}`
          : "SSE: connected";
      case "connecting":
        return "SSE: connecting";
      case "reconnecting":
        return "SSE: reconnecting";
      case "failed":
        return "SSE: failed";
      default:
        return "SSE: idle";
    }
  }
}

export const live = new LiveState();
