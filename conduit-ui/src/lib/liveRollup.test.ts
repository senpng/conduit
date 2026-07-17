import { describe, it, expect } from "vitest";
import {
  createRollup,
  applyEvent,
  upsertIndexRow,
  seedFromIndex,
  visibleRows,
  clearRollup,
  EMPTY_FILTER,
  type LiveRollup,
} from "./liveRollup";
import type { TraceEvent } from "./traceTypes";
import type { TraceIndexRow } from "./consoleClient";

function ev(traceId: string, kind: TraceEvent["kind"], ts = "2026-07-17T12:00:00Z"): TraceEvent {
  return { id: `e-${traceId}-${kind.type}`, trace_id: traceId, ts, kind };
}

function fullRequestLifecycle(r: LiveRollup, tid: string) {
  applyEvent(r, ev(tid, { type: "request_received", alias: "gpt-4o", stream: false }));
  applyEvent(
    r,
    ev(tid, {
      type: "routing_decided",
      provider_id: "p-openai",
      model_id: "gpt-4o",
      upstream_key_id: "k1",
      attempt_no: 0,
      attempt_loss: null,
    }),
  );
  applyEvent(r, ev(tid, { type: "upstream_response", status: 200, latency_ms: 842 }));
  applyEvent(
    r,
    ev(tid, {
      type: "final_usage",
      usage: { prompt_tokens: 10, completion_tokens: 20 },
      cost_usd: 0.0012,
      loss_report: null,
    }),
  );
}

describe("applyEvent", () => {
  it("merges all kinds of one request into a single row", () => {
    const r = createRollup();
    fullRequestLifecycle(r, "t1");
    expect(r.order).toEqual(["t1"]);
    const row = r.rows["t1"];
    expect(row.alias).toBe("gpt-4o");
    expect(row.providerId).toBe("p-openai");
    expect(row.modelId).toBe("gpt-4o");
    expect(row.status).toBe(200);
    expect(row.latencyMs).toBe(842);
    expect(row.costUsd).toBeCloseTo(0.0012);
    expect(row.eventCount).toBe(4);
    expect(row.lastKind).toBe("final_usage");
    expect(row.hasLoss).toBe(false);
  });

  it("falls back to event id when trace_id is empty", () => {
    const r = createRollup();
    applyEvent(r, {
      id: "only-id",
      trace_id: "",
      ts: "2026-07-17T12:00:00Z",
      kind: { type: "error", kind: "Timeout", message: "boom" },
    });
    expect(r.rows["only-id"].errorKind).toBe("Timeout");
  });

  it("flags loss from attempt_loss and loss_report", () => {
    const r = createRollup();
    applyEvent(
      r,
      ev("t-loss", {
        type: "routing_decided",
        provider_id: "p",
        model_id: "m",
        upstream_key_id: "k",
        attempt_loss: {
          warnings: [
            {
              field: "tool_choice",
              original: "AnyOf",
              degraded_to: "Required",
              reason: "downgraded",
            },
          ],
        },
      }),
    );
    expect(r.rows["t-loss"].hasLoss).toBe(true);

    applyEvent(
      r,
      ev("t-loss2", {
        type: "final_usage",
        usage: {},
        cost_usd: 0,
        loss_report: {
          warnings: [
            {
              field: "system",
              original: "x",
              degraded_to: "y",
              reason: "z",
            },
          ],
        },
      }),
    );
    expect(r.rows["t-loss2"].hasLoss).toBe(true);

    // Empty shell must not flag loss (daemon always attaches LossReport).
    applyEvent(
      r,
      ev("t-empty-loss", {
        type: "final_usage",
        usage: {},
        cost_usd: 0,
        loss_report: { warnings: [] },
      }),
    );
    expect(r.rows["t-empty-loss"].hasLoss).toBe(false);
  });

  it("does not fabricate status/latency with zeros", () => {
    const r = createRollup();
    applyEvent(r, ev("t2", { type: "request_received", alias: "a" }));
    const row = r.rows["t2"];
    expect(row.status).toBeUndefined();
    expect(row.latencyMs).toBeUndefined();
    expect(row.costUsd).toBeUndefined();
  });

  it("evicts oldest beyond cap", () => {
    const r = createRollup(3);
    for (const tid of ["a", "b", "c", "d"]) {
      applyEvent(r, ev(tid, { type: "request_received", alias: tid }));
    }
    expect(r.order).toEqual(["d", "c", "b"]);
    expect(r.rows["a"]).toBeUndefined();
    expect(r.rows["d"].alias).toBe("d");
  });
});

describe("upsertIndexRow / seedFromIndex", () => {
  function idx(partial: Partial<TraceIndexRow>): TraceIndexRow {
    return {
      id: "i1",
      trace_id: "t-idx",
      kind: "request_received",
      ts: "2026-07-17T12:00:00Z",
      alias: "gpt-4o",
      status_code: 0,
      latency_ms: 0,
      cost_usd: 0,
      ...partial,
    } as TraceIndexRow;
  }

  it("patches only fields carried by the row kind", () => {
    const r = createRollup();
    upsertIndexRow(r, idx({}));
    let row = r.rows["t-idx"];
    expect(row.alias).toBe("gpt-4o");
    expect(row.status).toBeUndefined(); // anchor row must not write 0

    upsertIndexRow(
      r,
      idx({ kind: "upstream_response", status_code: 200, latency_ms: 500 }),
    );
    upsertIndexRow(r, idx({ kind: "final_usage", cost_usd: 0.42 }));
    row = r.rows["t-idx"];
    expect(row.status).toBe(200);
    expect(row.latencyMs).toBe(500);
    expect(row.costUsd).toBeCloseTo(0.42);
    expect(row.eventCount).toBe(3);
  });

  it("seeds and dedupes with later SSE events by trace_id", () => {
    const r = createRollup();
    seedFromIndex(r, [idx({})]);
    applyEvent(
      r,
      ev("t-idx", { type: "upstream_response", status: 502, latency_ms: 90 }),
    );
    expect(r.order).toHaveLength(1);
    expect(r.rows["t-idx"].status).toBe(502);
  });
});

describe("visibleRows", () => {
  function seeded(): LiveRollup {
    const r = createRollup();
    fullRequestLifecycle(r, "ok-1"); // 200
    applyEvent(r, ev("err-1", { type: "request_received", alias: "claude" }));
    applyEvent(r, ev("err-1", { type: "error", kind: "RateLimited", message: "x" }));
    applyEvent(r, ev("flight-1", { type: "request_received", alias: "grok" }));
    applyEvent(
      r,
      ev("bad-1", { type: "upstream_response", status: 500, latency_ms: 10 }),
    );
    return r;
  }

  it("returns newest first with no filter", () => {
    const rows = visibleRows(seeded(), EMPTY_FILTER);
    expect(rows.map((x) => x.traceId)).toEqual(["bad-1", "flight-1", "err-1", "ok-1"]);
  });

  it("filters by status class, keeping in-flight when enabled", () => {
    const r = seeded();
    const ok = visibleRows(r, { ...EMPTY_FILTER, statusClass: "2xx" });
    expect(ok.map((x) => x.traceId)).toEqual(["flight-1", "ok-1"]);

    const noFlight = visibleRows(r, {
      text: "",
      statusClass: "2xx",
      includeInFlight: false,
    });
    expect(noFlight.map((x) => x.traceId)).toEqual(["ok-1"]);

    const err = visibleRows(r, { ...EMPTY_FILTER, statusClass: "err" });
    expect(err.map((x) => x.traceId)).toEqual(["bad-1", "flight-1", "err-1"]);

    const e5xx = visibleRows(r, {
      text: "",
      statusClass: "5xx",
      includeInFlight: false,
    });
    expect(e5xx.map((x) => x.traceId)).toEqual(["bad-1"]);
  });

  it("filters by text substring across alias/provider/traceId", () => {
    const r = seeded();
    expect(visibleRows(r, { ...EMPTY_FILTER, text: "claude" }).map((x) => x.traceId)).toEqual([
      "err-1",
    ]);
    expect(visibleRows(r, { ...EMPTY_FILTER, text: "OK-1" }).map((x) => x.traceId)).toEqual([
      "ok-1",
    ]);
    expect(visibleRows(r, { ...EMPTY_FILTER, text: "openai" }).map((x) => x.traceId)).toEqual([
      "ok-1",
    ]);
  });

  it("clear resets", () => {
    const r = seeded();
    clearRollup(r);
    expect(visibleRows(r, EMPTY_FILTER)).toEqual([]);
  });
});
