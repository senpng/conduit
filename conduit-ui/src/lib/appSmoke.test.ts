// @vitest-environment jsdom
/**
 * Runtime smoke test: mounts the full App shell in jsdom and exercises
 * navigation across all screens. Catches Svelte runes misuse, broken imports,
 * and mount-time crashes that `vite build` (esbuild, no typecheck) and the
 * pure-logic unit tests cannot see. No daemon is required — failed health
 * polls are part of the exercised paths.
 */

import { describe, it, expect, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import App from "../App.svelte";
import { app, SCREENS } from "../state/app.svelte";

describe("App smoke", () => {
  let instance: Record<string, unknown> | null = null;

  afterEach(() => {
    if (instance) {
      unmount(instance);
      instance = null;
    }
    document.body.innerHTML = "";
  });

  it("mounts the shell with sidebar navigation", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(App, { target }) as Record<string, unknown>;
    flushSync();

    expect(document.querySelector(".sidebar")).toBeTruthy();
    expect(document.querySelectorAll(".nav-item").length).toBe(SCREENS.length);
    expect(document.querySelector(".page-title")?.textContent).toBe("Dashboard");
  });

  it("navigates through every screen without crashing", async () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(App, { target }) as Record<string, unknown>;
    flushSync();

    for (const s of SCREENS) {
      app.goto(s.id);
      flushSync();
      await new Promise((r) => setTimeout(r, 0));
      expect(document.querySelector(".page-title")?.textContent).toBe(s.label);
    }
    app.goto("dashboard");
    flushSync();
    expect(app.screen).toBe("dashboard");
  });

  it("shows confirm dialog via promise API and settles", async () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(App, { target }) as Record<string, unknown>;
    flushSync();

    const p = app.askConfirm({ title: "Delete x?", body: "cannot be undone" });
    flushSync();
    expect(document.querySelector(".modal")).toBeTruthy();
    app.settleConfirm(false);
    flushSync();
    expect(await p).toBe(false);
    expect(document.querySelector(".modal")).toBeNull();
  });
});
