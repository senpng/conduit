import { describe, it, expect } from "vitest";
import {
  controlClass,
  tableWrapClass,
  thClass,
  segmentBtnActiveClass,
} from "./ui";

describe("ui class helpers", () => {
  it("exposes light semantic token utilities for forms and tables", () => {
    expect(controlClass).toContain("border-[var(--border)]");
    expect(controlClass).toContain("bg-[var(--surface)]");
    expect(tableWrapClass).toContain("bg-[var(--surface)]");
    expect(thClass).toContain("bg-[var(--surface-muted)]");
    expect(segmentBtnActiveClass).toContain("bg-[var(--accent-soft)]");
    expect(segmentBtnActiveClass).toContain("text-[var(--accent)]");
  });
});
