import { describe, it, expect } from "vitest";
import {
  extractSseData,
  extractSseEventName,
  consumeSseChunk,
  consumeSseFrames,
  parseLaggedSkipped,
  isStubTailPayload,
} from "./sse";

describe("extractSseData", () => {
  it("parses real admin SSE data payload with id and kind", () => {
    const frame =
      'event: message\ndata: {"id":"01TRACE","kind":{"type":"request_received","alias":"gpt-4o"}}\n';
    const data = extractSseData(frame);
    expect(data).not.toBeNull();
    const v = JSON.parse(data!);
    expect(v.id).toBe("01TRACE");
    expect(v.kind.type).toBe("request_received");
    expect(isStubTailPayload(data!)).toBe(false);
  });

  it("joins multiline data fields", () => {
    expect(extractSseData("data: a\ndata: b\n")).toBe("a\nb");
  });
});

describe("extractSseEventName", () => {
  it("defaults to message when no event field", () => {
    expect(extractSseEventName('data: {"a":1}\n')).toBe("message");
  });

  it("reads explicit event names", () => {
    expect(extractSseEventName('event: lagged\ndata: {"skipped":3}\n')).toBe(
      "lagged",
    );
  });
});

describe("consumeSseChunk", () => {
  it("emits payloads across chunk boundaries", () => {
    let state = { buffer: "", payloads: [] as string[] };
    state = {
      ...consumeSseChunk(state.buffer, 'data: {"id":"1"'),
      payloads: [],
    };
    const next = consumeSseChunk(state.buffer, "}\n\ndata: {\"id\":\"2\"}\n\n");
    expect(next.payloads).toHaveLength(2);
    expect(JSON.parse(next.payloads[0]).id).toBe("1");
    expect(JSON.parse(next.payloads[1]).id).toBe("2");
  });
});

describe("consumeSseFrames (event-aware)", () => {
  it("separates trace frames from lagged frames", () => {
    const wire =
      'data: {"id":"e1","kind":{"type":"request_received","alias":"a"}}\n\n' +
      'event: lagged\ndata: {"skipped":7}\n\n';
    const out = consumeSseFrames("", wire);
    expect(out.frames).toHaveLength(2);
    expect(out.frames[0].event).toBe("message");
    expect(out.frames[1].event).toBe("lagged");
    expect(parseLaggedSkipped(out.frames[1].data)).toBe(7);
  });

  it("matches daemon format_lagged_sse_frame wire shape", () => {
    // crates/conduitd/src/admin.rs: "event: lagged\ndata: {\"skipped\":N}\n\n"
    const frame = 'event: lagged\ndata: {"skipped":1024}\n\n';
    const out = consumeSseFrames("", frame);
    expect(out.frames).toHaveLength(1);
    expect(out.frames[0].event).toBe("lagged");
    expect(parseLaggedSkipped(out.frames[0].data)).toBe(1024);
  });

  it("handles CRLF line endings", () => {
    const out = consumeSseFrames("", 'data: {"id":"x"}\r\n\r\n');
    expect(out.frames).toHaveLength(1);
  });
});

describe("parseLaggedSkipped", () => {
  it("returns 0 on malformed body", () => {
    expect(parseLaggedSkipped("not json")).toBe(0);
    expect(parseLaggedSkipped("{}")).toBe(0);
  });
});

describe("isStubTailPayload", () => {
  it("flags old mock banner text", () => {
    expect(isStubTailPayload("Real-time tail not yet implemented")).toBe(true);
  });
});
