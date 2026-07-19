/**
 * Structural checks for the rewrite stack: light-only tokens, local fonts,
 * and shell IA. Drives real source/config files rather than re-stating values.
 */

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { SCREENS, SCREEN_GROUPS } from "../state/app.svelte";

const root = resolve(import.meta.dirname, "../..");

function read(rel: string): string {
  return readFileSync(resolve(root, rel), "utf8");
}

describe("rewrite stack + light tokens", () => {
  it("declares Tailwind, lucide, and bits-ui dependencies", () => {
    const pkg = JSON.parse(read("package.json")) as {
      dependencies?: Record<string, string>;
      devDependencies?: Record<string, string>;
    };
    const all = { ...pkg.dependencies, ...pkg.devDependencies };
    expect(all["tailwindcss"]).toBeTruthy();
    expect(all["@tailwindcss/vite"]).toBeTruthy();
    expect(all["@lucide/svelte"]).toBeTruthy();
    expect(all["bits-ui"]).toBeTruthy();
    expect(all["clsx"]).toBeTruthy();
    expect(all["tailwind-merge"]).toBeTruthy();
  });

  it("wires Tailwind into Vite and CSS entry", () => {
    const vite = read("vite.config.ts");
    const css = read("src/app.css");
    expect(vite).toMatch(/@tailwindcss\/vite/);
    expect(vite).toMatch(/tailwindcss\(\)/);
    expect(css).toMatch(/@import\s+["']tailwindcss["']/);
  });

  it("defines light-only semantic design tokens", () => {
    const css = read("src/app.css");
    for (const token of [
      "--background",
      "--surface",
      "--accent",
      "--text",
      "--border",
      "--success",
      "--warning",
      "--danger",
    ]) {
      expect(css.includes(token)).toBe(true);
    }
    // Soft gray canvas + white surfaces + accent blue from design doc.
    expect(css).toMatch(/--background:\s*#f4f6fa/i);
    expect(css).toMatch(/--surface:\s*#ffffff/i);
    expect(css).toMatch(/--accent:\s*#2563eb/i);
    expect(css).toMatch(/color-scheme:\s*light/);
    // No dark-default prefers-color-scheme theme.
    expect(css).not.toMatch(/prefers-color-scheme\s*:\s*dark/);
    expect(css).not.toMatch(/prefers-color-scheme/);
  });

  it("uses system font stacks only (no remote font/icon CDNs)", () => {
    const css = read("src/app.css");
    const app = read("src/App.svelte");
    const index = read("index.html");
    const blob = `${css}\n${app}\n${index}\n${read("package.json")}`;
    expect(blob).not.toMatch(/fonts\.googleapis\.com/);
    expect(blob).not.toMatch(/fonts\.gstatic\.com/);
    expect(blob).not.toMatch(/unpkg\.com/);
    expect(blob).not.toMatch(/cdn\.jsdelivr\.net/);
    expect(css).toMatch(/-apple-system|BlinkMacSystemFont|Segoe UI/);
    expect(css).toMatch(/ui-monospace|SF Mono|Menlo/);
    // Must not advertise dark color-scheme (would make native controls look "old dark").
    expect(index).toMatch(/color-scheme" content="light"/);
    expect(index).not.toMatch(/content="dark/);
  });

  it("exposes rewritten IA with Settings and Usage, without top-level Pricing", () => {
    const ids = SCREENS.map((s) => s.id);
    expect(ids).toContain("settings");
    expect(ids).toContain("usage");
    expect(ids).not.toContain("pricing");
    expect(ids).toEqual(
      expect.arrayContaining([
        "dashboard",
        "usage",
        "providers",
        "routes",
        "keys",
        "settings",
      ]),
    );
    expect(SCREEN_GROUPS.map((g) => g.id)).toEqual([
      "overview",
      "observability",
      "configuration",
      "system",
    ]);
  });

  it("ships project-owned UI primitives and cn helper", () => {
    expect(read("src/lib/utils.ts")).toMatch(/export function cn/);
    expect(read("src/components/ui/button.svelte")).toMatch(/buttonVariants|cva/);
    expect(read("src/components/ui/badge.svelte")).toMatch(/badgeVariants|cva/);
    expect(read("src/App.svelte")).toMatch(/from "@lucide\/svelte"/);
    expect(read("src/App.svelte")).toMatch(/class="sidebar/);
    expect(read("src/App.svelte")).toMatch(/page-title/);
  });

  it("rewrites feature views onto light composites rather than legacy .btn-primary chrome", () => {
    const views = [
      "src/views/DashboardView.svelte",
      "src/views/UsageView.svelte",
      "src/views/PricingView.svelte",
      "src/views/ProvidersView.svelte",
      "src/views/RoutesView.svelte",
      "src/views/RouteWizard.svelte",
      "src/views/KeysView.svelte",
      "src/views/SettingsView.svelte",
      "src/views/OAuthPanel.svelte",
    ];
    for (const v of views) {
      const src = read(v);
      // Primary chrome is light system: composites, Button, or $lib/ui helpers.
      const usesLightStack =
        src.includes("components/app/") ||
        src.includes("components/ui/") ||
        src.includes("$lib/ui") ||
        src.includes("var(--accent)") ||
        src.includes("var(--surface)");
      expect(usesLightStack, `${v} should use light stack`).toBe(true);
      // Must not be primarily the pre-rewrite dark hand classes.
      expect(src.includes('class="btn-primary"'), `${v} legacy btn-primary`).toBe(
        false,
      );
      expect(src.includes('class="panel"'), `${v} legacy panel class`).toBe(false);
    }
    // Shared composites exist.
    expect(read("src/components/app/Card.svelte")).toMatch(/var\(--surface\)/);
    expect(read("src/components/app/Alert.svelte")).toMatch(/variant/);
    expect(read("src/lib/ui.ts")).toMatch(/tableWrapClass/);
  });
});
