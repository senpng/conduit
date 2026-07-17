/**
 * App-wide UI state: screen nav, health polling, toasts, global keyboard.
 * Svelte 5 runes in a singleton class; lifecycle is explicit (start/stop
 * from App.svelte onMount) so no $effect roots leak.
 */

import { health as healthApi, getConsoleBase } from "../lib/consoleClient";
import type { HealthResponse } from "../lib/consoleClient";

export type ScreenId =
  | "dashboard"
  | "live"
  | "traces"
  | "providers"
  | "routes"
  | "keys"
  | "usage"
  | "pricing";

export interface ScreenMeta {
  id: ScreenId;
  label: string;
  icon: string;
  /** g-prefix jump key. */
  g: string;
}

export const SCREENS: ScreenMeta[] = [
  { id: "dashboard", label: "Dashboard", icon: "▤", g: "d" },
  { id: "live", label: "Live Monitor", icon: "◉", g: "m" },
  { id: "traces", label: "Traces", icon: "☰", g: "t" },
  { id: "providers", label: "Providers", icon: "◈", g: "p" },
  { id: "routes", label: "Routes", icon: "⟶", g: "r" },
  { id: "keys", label: "Keys", icon: "⚿", g: "k" },
  { id: "usage", label: "Usage", icon: "◎", g: "b" },
  { id: "pricing", label: "Pricing", icon: "◇", g: "i" },
];

export interface Toast {
  id: number;
  kind: "info" | "ok" | "warn" | "error";
  text: string;
}

export interface ConfirmRequest {
  title: string;
  body: string;
  confirmLabel: string;
  danger: boolean;
  resolve: (ok: boolean) => void;
}

class AppState {
  screen = $state<ScreenId>("dashboard");
  health = $state<HealthResponse | null>(null);
  healthError = $state(false);
  rttMs = $state<number | null>(null);
  paletteOpen = $state(false);
  helpOpen = $state(false);
  toasts = $state<Toast[]>([]);
  confirm = $state<ConfirmRequest | null>(null);
  /** Deep-link: trace id to open when Traces screen mounts. */
  traceFocus = $state<string | null>(null);

  private healthTimer: ReturnType<typeof setInterval> | null = null;
  private toastSeq = 0;
  private gPrefixAt = 0;
  private refreshers = new Map<ScreenId, () => void>();

  readonly consoleBase = getConsoleBase();
  readonly isLoopback = /^https?:\/\/(127\.0\.0\.1|localhost|\[::1\])(:|\/|$)/.test(
    this.consoleBase,
  );

  start(): void {
    void this.pollHealth();
    this.healthTimer = setInterval(() => void this.pollHealth(), 5000);
  }

  stop(): void {
    if (this.healthTimer) clearInterval(this.healthTimer);
    this.healthTimer = null;
  }

  private async pollHealth(): Promise<void> {
    const t0 = performance.now();
    try {
      this.health = await healthApi.check();
      this.healthError = false;
      this.rttMs = Math.round(performance.now() - t0);
    } catch {
      this.health = null;
      this.healthError = true;
      this.rttMs = null;
    }
  }

  goto(id: ScreenId): void {
    this.screen = id;
  }

  openTrace(traceId: string): void {
    this.traceFocus = traceId;
    this.screen = "traces";
  }

  toast(text: string, kind: Toast["kind"] = "info", ms = 4000): void {
    const id = ++this.toastSeq;
    this.toasts = [...this.toasts, { id, kind, text }];
    setTimeout(() => this.dismissToast(id), ms);
  }

  dismissToast(id: number): void {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  /** Promise-based confirm; ConfirmModal renders `app.confirm`. */
  askConfirm(req: {
    title: string;
    body: string;
    confirmLabel?: string;
    danger?: boolean;
  }): Promise<boolean> {
    return new Promise((resolve) => {
      this.confirm = {
        title: req.title,
        body: req.body,
        confirmLabel: req.confirmLabel ?? "Confirm",
        danger: req.danger ?? true,
        resolve,
      };
    });
  }

  settleConfirm(ok: boolean): void {
    this.confirm?.resolve(ok);
    this.confirm = null;
  }

  registerRefresher(id: ScreenId, fn: () => void): () => void {
    this.refreshers.set(id, fn);
    return () => this.refreshers.delete(id);
  }

  refreshCurrent(): void {
    this.refreshers.get(this.screen)?.();
  }

  /** True when any overlay owns the keyboard. */
  get modalActive(): boolean {
    return this.paletteOpen || this.helpOpen || this.confirm != null;
  }

  /**
   * Global key handler installed once by App.svelte. Returns early when an
   * overlay or a text input owns the keyboard.
   */
  handleKey = (e: KeyboardEvent): void => {
    if (e.key === "k" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      this.paletteOpen = !this.paletteOpen;
      return;
    }
    if (this.modalActive) return; // overlays handle their own keys

    const el = document.activeElement as HTMLElement | null;
    const tag = el?.tagName;
    const inField =
      tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el?.isContentEditable;
    if (inField) {
      if (e.key === "Escape") el?.blur();
      return;
    }

    if (e.key === "Escape") return; // nothing to close
    if (e.key === "?") {
      e.preventDefault();
      this.helpOpen = true;
      return;
    }
    if (e.key === "r" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      this.refreshCurrent();
      return;
    }

    // g-prefix: first `g` arms (1s window), second key jumps.
    const now = Date.now();
    if (e.key === "g" && !e.ctrlKey && !e.metaKey) {
      if (now - this.gPrefixAt < 1000) {
        // gg → scroll top of main pane
        this.gPrefixAt = 0;
        document.querySelector(".view-scroll")?.scrollTo({ top: 0 });
      } else {
        this.gPrefixAt = now;
      }
      return;
    }
    if (now - this.gPrefixAt < 1000) {
      this.gPrefixAt = 0;
      const meta = SCREENS.find((s) => s.g === e.key);
      if (meta) {
        e.preventDefault();
        this.goto(meta.id);
      }
    }
  };
}

export const app = new AppState();
