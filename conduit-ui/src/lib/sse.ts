/**
 * SSE frame helpers for the console trace stream (`GET /console/traces/stream`).
 *
 * Frames come in two shapes:
 *   - default / `event: message` + `data: <TraceEvent JSON>`
 *   - `event: lagged` + `data: {"skipped":N}`   (daemon KD-13 / PR1b)
 */

/** Extract concatenated `data:` fields from one SSE event frame. */
export function extractSseData(frame: string): string | null {
  const dataLines: string[] = [];
  for (const line of frame.split(/\r?\n/)) {
    if (line.startsWith("data:")) {
      dataLines.push(line.slice(5).replace(/^\s/, ""));
    }
  }
  if (dataLines.length === 0) return null;
  return dataLines.join("\n");
}

/** Extract the `event:` field from one SSE frame ("message" when absent). */
export function extractSseEventName(frame: string): string {
  for (const line of frame.split(/\r?\n/)) {
    if (line.startsWith("event:")) {
      return line.slice(6).replace(/^\s/, "").trim() || "message";
    }
  }
  return "message";
}

export interface SseFrame {
  /** SSE event name; "message" for default frames. */
  event: string;
  data: string;
}

/**
 * Incremental SSE buffer consumer. Call with each text chunk; returns
 * completed frames (one per event terminated by a blank line).
 */
export function consumeSseFrames(
  buffer: string,
  chunk: string,
): { buffer: string; frames: SseFrame[] } {
  let buf = buffer + chunk;
  const frames: SseFrame[] = [];
  // SSE events end with \n\n (or \r\n\r\n)
  while (true) {
    const idx = buf.search(/\r?\n\r?\n/);
    if (idx < 0) break;
    const frame = buf.slice(0, idx);
    // advance past the blank line
    const match = buf.slice(idx).match(/^(\r?\n){2}/);
    const skip = match ? match[0].length : 2;
    buf = buf.slice(idx + skip);
    const data = extractSseData(frame);
    if (data != null && data.length > 0) {
      frames.push({ event: extractSseEventName(frame), data });
    }
  }
  return { buffer: buf, frames };
}

/**
 * Back-compat wrapper: data payloads only (no event names).
 */
export function consumeSseChunk(
  buffer: string,
  chunk: string,
): { buffer: string; payloads: string[] } {
  const out = consumeSseFrames(buffer, chunk);
  return { buffer: out.buffer, payloads: out.frames.map((f) => f.data) };
}

/** Parse a `lagged` frame body: `{"skipped":N}` → N (0 when malformed). */
export function parseLaggedSkipped(data: string): number {
  try {
    const v = JSON.parse(data) as { skipped?: unknown };
    return typeof v.skipped === "number" ? v.skipped : 0;
  } catch {
    return 0;
  }
}

/** Reject known stub / mock banners from older CLI implementations. */
export function isStubTailPayload(payload: string): boolean {
  return /not yet implemented/i.test(payload);
}
