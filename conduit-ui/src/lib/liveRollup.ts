/**
 * Live request rollup — pure, framework-free merge logic for the Live Monitor.
 *
 * One table row = one gateway request (shared trace_id). Events of all kinds
 * are patched into the row per `docs/design/conduit-ui-rewrite.md` (R-3),
 * one row per gateway request.
 */

import type { TraceEvent } from "./traceTypes";
import { lossReportNonEmpty } from "./traceTypes";
import type { TraceIndexRow } from "./adminClient";

export interface LiveRequestRow {
  traceId: string;
  alias?: string;
  providerId?: string;
  modelId?: string;
  /** HTTP status from upstream_response; undefined while in flight. */
  status?: number;
  errorKind?: string;
  latencyMs?: number;
  costUsd?: number;
  hasLoss: boolean;
  /** Type tag of the most recent event (in-flight indicator). */
  lastKind: string;
  updatedAt: string;
  eventCount: number;
  /** Accumulated assistant text from live stream_delta events. */
  streamText?: string;
  /** True while stream_delta events are arriving. */
  streaming?: boolean;
}

export interface LiveRollup {
  /** trace_id list, newest first. */
  order: string[];
  rows: Record<string, LiveRequestRow>;
  cap: number;
}

export const DEFAULT_ROLLUP_CAP = 1000;

export function createRollup(cap: number = DEFAULT_ROLLUP_CAP): LiveRollup {
  return { order: [], rows: {}, cap };
}

export function clearRollup(r: LiveRollup): void {
  r.order = [];
  r.rows = {};
}

function touch(r: LiveRollup, tid: string, ts: string): LiveRequestRow {
  let row = r.rows[tid];
  if (!row) {
    row = {
      traceId: tid,
      hasLoss: false,
      lastKind: "",
      updatedAt: ts,
      eventCount: 0,
    };
    r.rows[tid] = row;
    r.order.unshift(tid);
    // Evict oldest beyond cap.
    while (r.order.length > r.cap) {
      const evict = r.order.pop();
      if (evict != null) delete r.rows[evict];
    }
  }
  if (ts > row.updatedAt) row.updatedAt = ts;
  row.eventCount += 1;
  return row;
}

/** Merge one live SSE TraceEvent into the rollup (upsert by trace_id). */
export function applyEvent(r: LiveRollup, ev: TraceEvent): void {
  const tid = ev.trace_id || ev.id;
  if (!tid) return;
  const row = touch(r, tid, ev.ts ?? "");
  const kind = ev.kind;
  row.lastKind = kind.type;
  switch (kind.type) {
    case "request_received":
      row.alias = kind.alias;
      break;
    case "routing_decided":
      row.providerId = kind.provider_id;
      row.modelId = kind.model_id;
      if (lossReportNonEmpty(kind.attempt_loss)) row.hasLoss = true;
      break;
    case "stream_delta":
      row.streaming = true;
      if (kind.text_delta) {
        row.streamText = (row.streamText ?? "") + kind.text_delta;
      }
      break;
    case "upstream_response":
      row.status = kind.status;
      row.latencyMs = kind.latency_ms;
      row.streaming = false;
      break;
    case "final_usage":
      row.costUsd = kind.cost_usd;
      if (lossReportNonEmpty(kind.loss_report)) row.hasLoss = true;
      break;
    case "error":
      row.errorKind = kind.kind;
      break;
    default:
      // Unknown kind: lastKind/eventCount already updated; never fabricate fields.
      break;
  }
}

/**
 * Merge one trace-index row (from `GET /admin/traces`) into the rollup.
 * Index rows are per-event and flat; patch only the fields the row's `kind`
 * actually carries (a `request_received` anchor has status_code 0, which must
 * NOT clobber a real status merged earlier).
 */
export function upsertIndexRow(r: LiveRollup, idx: TraceIndexRow): void {
  const tid = idx.trace_id || idx.id;
  if (!tid) return;
  const row = touch(r, tid, idx.ts ?? "");
  if (idx.kind) row.lastKind = idx.kind;
  if (idx.alias) row.alias = idx.alias;
  if (idx.provider_id) row.providerId = idx.provider_id;
  if (idx.model_id) row.modelId = idx.model_id;
  if (idx.kind === "upstream_response") {
    if (idx.status_code) row.status = idx.status_code;
    if (idx.latency_ms) row.latencyMs = idx.latency_ms;
  }
  if (idx.kind === "final_usage" && idx.cost_usd != null) {
    row.costUsd = idx.cost_usd;
  }
  if (idx.kind === "error" && idx.error_kind) {
    row.errorKind = idx.error_kind;
  }
}

/** Seed the rollup from index rows (any order; merged by trace_id). */
export function seedFromIndex(r: LiveRollup, rows: TraceIndexRow[]): void {
  // Chronological fold keeps lastKind/updatedAt monotonic-ish.
  const sorted = [...rows].sort((a, b) =>
    (a.ts ?? "") < (b.ts ?? "") ? -1 : 1,
  );
  for (const row of sorted) upsertIndexRow(r, row);
}

export type StatusClass = "" | "2xx" | "4xx" | "5xx" | "err";

export interface LiveFilter {
  /** Case-insensitive substring on traceId / alias / provider / model. */
  text: string;
  statusClass: StatusClass;
  /** Show rows with no status and no error yet (default true). */
  includeInFlight: boolean;
}

export const EMPTY_FILTER: LiveFilter = {
  text: "",
  statusClass: "",
  includeInFlight: true,
};

function matchesStatus(row: LiveRequestRow, sc: StatusClass): boolean {
  if (!sc) return true;
  if (sc === "err") {
    // Errored = gateway error event, or upstream 5xx.
    return row.errorKind != null || (row.status != null && row.status >= 500);
  }
  if (row.errorKind != null) return false;
  if (row.status == null) return false;
  const lo = Number(sc[0]) * 100;
  return row.status >= lo && row.status < lo + 100;
}

function matchesText(
  row: LiveRequestRow,
  text: string,
  /** Optional id → display name so filters can match human names. */
  providerNameOf?: (id: string) => string | undefined,
): boolean {
  if (!text) return true;
  const t = text.toLowerCase();
  const fields: (string | undefined)[] = [
    row.traceId,
    row.alias,
    row.providerId,
    row.modelId,
  ];
  if (row.providerId && providerNameOf) {
    fields.push(providerNameOf(row.providerId));
  }
  return fields.some((f) => f != null && f.toLowerCase().includes(t));
}

/** Filtered rows in display order (newest first). */
export function visibleRows(
  r: LiveRollup,
  f: LiveFilter,
  providerNameOf?: (id: string) => string | undefined,
): LiveRequestRow[] {
  const out: LiveRequestRow[] = [];
  for (const tid of r.order) {
    const row = r.rows[tid];
    if (!row) continue;
    const inFlight = row.status == null && row.errorKind == null;
    if (f.statusClass && inFlight) {
      if (!f.includeInFlight) continue;
    } else if (!matchesStatus(row, f.statusClass)) {
      continue;
    }
    if (!matchesText(row, f.text, providerNameOf)) continue;
    out.push(row);
  }
  return out;
}
