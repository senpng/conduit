import { describe, it, expect } from "vitest";
import { fuzzyMatch, fuzzyFilter } from "./fuzzy";

describe("fuzzyMatch", () => {
  it("matches subsequence case-insensitively", () => {
    expect(fuzzyMatch("lv", "Live Monitor")).not.toBeNull();
    expect(fuzzyMatch("LVM", "Live Monitor")).not.toBeNull();
    expect(fuzzyMatch("xyz", "Live Monitor")).toBeNull();
  });

  it("prefers word-start hits", () => {
    const a = fuzzyMatch("mon", "Monitor")!;
    const b = fuzzyMatch("mon", "salmon run")!;
    expect(a.score).toBeGreaterThan(b.score);
  });

  it("prefers consecutive runs", () => {
    const a = fuzzyMatch("trac", "Traces")!;
    const b = fuzzyMatch("trac", "t r a c")!;
    expect(a.score).toBeGreaterThan(b.score);
  });

  it("empty query matches everything with zero score", () => {
    expect(fuzzyMatch("", "anything")).toEqual({ score: 0, indices: [] });
  });
});

describe("fuzzyFilter", () => {
  const cmds = ["Go to Dashboard", "Go to Live Monitor", "Go to Traces", "Refresh", "Quit"];

  it("ranks best match first", () => {
    const out = fuzzyFilter("trace", cmds, (x) => x);
    expect(out[0].item).toBe("Go to Traces");
  });

  it("returns all in order for empty query", () => {
    expect(fuzzyFilter("", cmds, (x) => x).map((x) => x.item)).toEqual(cmds);
  });

  it("drops non-matches", () => {
    const out = fuzzyFilter("zzz", cmds, (x) => x);
    expect(out).toEqual([]);
  });
});
