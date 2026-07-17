import { describe, expect, it } from "vitest";
import { providerDisplayName, providerNameMap } from "./format";

describe("providerDisplayName", () => {
  const list = [
    { id: "01ABC", name: "tui-codex" },
    { id: "01DEF", name: "  " },
    { id: "01GHI", name: null },
  ];

  it("prefers name over id", () => {
    expect(providerDisplayName(list, "01ABC")).toBe("tui-codex");
  });

  it("falls back to id when name empty or missing", () => {
    expect(providerDisplayName(list, "01DEF")).toBe("01DEF");
    expect(providerDisplayName(list, "01GHI")).toBe("01GHI");
    expect(providerDisplayName(list, "unknown")).toBe("unknown");
  });

  it("returns em dash for empty id", () => {
    expect(providerDisplayName(list, null)).toBe("—");
    expect(providerDisplayName(list, "")).toBe("—");
  });
});

describe("providerNameMap", () => {
  it("only stores non-empty names", () => {
    const m = providerNameMap([
      { id: "a", name: "Alpha" },
      { id: "b", name: "" },
    ]);
    expect(m.get("a")).toBe("Alpha");
    expect(m.has("b")).toBe(false);
  });
});
