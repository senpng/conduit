/**
 * Types mirroring daemon trace contract (`conduit-ir/src/trace.rs`,
 * `conduitd/src/console.rs`). SSE frames carry the full TraceEvent JSON with an
 * internally-tagged `kind` (`kind.type`).
 */

export interface LossReportEntry {
  field?: string;
  reason?: string;
  original?: string;
  degraded_to?: string;
  [k: string]: unknown;
}

/** Matches conduit-ir `LossReport` (`warnings` array). */
export interface LossReport {
  /** Canonical field from Rust LossReport. */
  warnings?: LossReportEntry[];
  /** Legacy / alternate shape. */
  entries?: LossReportEntry[];
  [k: string]: unknown;
}

export interface Usage {
  prompt_tokens?: number;
  completion_tokens?: number;
  reasoning_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  [k: string]: number | undefined;
}

export type TraceEventKind =
  | {
      type: "request_received";
      downstream_key_id?: string | null;
      alias: string;
      stream?: boolean;
      /** Original client wire body. */
      request?: unknown;
      /** Canonical IR after decode. */
      request_ir?: unknown;
      wire_format?: string | null;
      /** Client request headers (secrets redacted). */
      request_headers?: Record<string, string | string[]> | null;
    }
  | {
      type: "routing_decided";
      provider_id: string;
      model_id: string;
      upstream_key_id?: string;
      attempt_no?: number;
      /** Static route-target fields merged into this upstream attempt. */
      request_overrides?: Record<string, unknown>;
      attempt_loss?: LossReport | null;
    }
  | {
      type: "stream_delta";
      seq: number;
      /** Exact SSE frame text as delivered to the client. */
      frame: string;
      text_delta?: string | null;
    }
  | {
      type: "upstream_response";
      status: number;
      latency_ms: number;
      ttfb_ms?: number | null;
      /** Wire body (non-stream) or stream_summary object. */
      response?: unknown;
      wire_format?: string | null;
      stream?: boolean;
      /** Exact SSE frames as sent to the client. */
      stream_frames?: string[] | null;
      /** Client-facing response headers set by the gateway. */
      response_headers?: Record<string, string | string[]> | null;
      /** Exact JSON body sent upstream after codec and route transforms. */
      upstream_request_headers?: Record<string, string | string[]> | null;
      upstream_response_headers?: Record<string, string | string[]> | null;
    }
  | {
      type: "final_usage";
      usage: Usage;
      cost_usd: number;
      loss_report?: LossReport | null;
      downstream_key_id?: string | null;
    }
  | { type: "error"; kind: string; message: string };

/** Full event JSON pushed on `GET /console/traces/stream`. */
export interface TraceEvent {
  id: string;
  trace_id?: string;
  ts: string;
  kind: TraceEventKind;
}

/** Complete audit bundle from `GET /console/traces/{id}`. */
export interface TraceBundle {
  trace_id: string;
  events: TraceEvent[];
  /** Original client wire body. */
  request?: unknown;
  /** Canonical IR (when recorded). */
  request_ir?: unknown;
  request_headers?: Record<string, string | string[]> | null;
  response?: unknown;
  response_headers?: Record<string, string | string[]> | null;
  wire_format?: string | null;
  stream?: boolean;
  stream_frames?: string[] | null;
}

/**
 * True only when the report contains at least one real degradation.
 *
 * Empty shell objects like `{ "warnings": [] }` must not count — daemon always
 * attaches a LossReport, often empty.
 */
export function lossReportNonEmpty(loss: unknown): boolean {
  if (loss == null) return false;
  if (Array.isArray(loss)) return loss.length > 0;
  if (typeof loss !== "object") return false;

  const obj = loss as LossReport;
  // Prefer the Rust wire field name.
  if (Array.isArray(obj.warnings)) return obj.warnings.length > 0;
  if (Array.isArray(obj.entries)) return obj.entries.length > 0;

  // Unknown object shapes: non-empty only if there is a non-array value worth
  // showing (avoid treating empty arrays / nulls as signal).
  for (const v of Object.values(obj)) {
    if (Array.isArray(v)) {
      if (v.length > 0) return true;
      continue;
    }
    if (v != null && v !== "" && typeof v !== "object") return true;
    if (v != null && typeof v === "object" && !Array.isArray(v) && Object.keys(v).length > 0) {
      return true;
    }
  }
  return false;
}

/** Parse a raw SSE `data:` payload into a TraceEvent, or null if malformed. */
export function parseTraceEvent(data: string): TraceEvent | null {
  try {
    const v = JSON.parse(data) as TraceEvent;
    if (!v || typeof v !== "object" || !v.kind || typeof v.kind !== "object") {
      return null;
    }
    if (typeof (v.kind as { type?: unknown }).type !== "string") return null;
    return v;
  } catch {
    return null;
  }
}
