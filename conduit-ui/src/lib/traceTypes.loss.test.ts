import { describe, it, expect } from "vitest";
import { lossReportNonEmpty } from "./traceTypes";

describe("lossReportNonEmpty", () => {
  it("treats empty warnings shell as empty", () => {
    expect(lossReportNonEmpty({ warnings: [] })).toBe(false);
  });

  it("treats empty entries shell as empty", () => {
    expect(lossReportNonEmpty({ entries: [] })).toBe(false);
  });

  it("detects real warnings from Rust LossReport", () => {
    expect(
      lossReportNonEmpty({
        warnings: [
          {
            field: "tool_choice",
            original: "AnyOf",
            degraded_to: "Required",
            reason: "unsupported",
          },
        ],
      }),
    ).toBe(true);
  });

  it("detects legacy entries shape", () => {
    expect(
      lossReportNonEmpty({ entries: [{ field: "tool_choice", reason: "downgraded" }] }),
    ).toBe(true);
  });

  it("null / undefined are empty", () => {
    expect(lossReportNonEmpty(null)).toBe(false);
    expect(lossReportNonEmpty(undefined)).toBe(false);
  });
});
