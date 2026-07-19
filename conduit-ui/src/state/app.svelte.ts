/**
 * App-wide UI state: screen nav, health polling, toasts, confirms.
 * Svelte 5 runes in a singleton class; lifecycle is explicit (start/stop
 * from App.svelte onMount) so no $effect roots leak.
 *
 * No global keyboard shortcuts — navigation and actions are pointer/UI only.
 */

import { health as healthApi, getConsoleBase } from "../lib/consoleClient";
import type { HealthResponse } from "../lib/consoleClient";

export type ScreenId =
  | "dashboard"
  | "usage"
  | "providers"
  | "routes"
  | "keys"
  | "settings";

export type ScreenGroup =
  | "overview"
  | "observability"
  | "configuration"
  | "system";

export interface ScreenMeta {
  id: ScreenId;
  label: string;
  group: ScreenGroup;
}

export const SCREENS: ScreenMeta[] = [
  { id: "dashboard", label: "Dashboard", group: "overview" },
  { id: "usage", label: "Usage", group: "observability" },
  { id: "providers", label: "Providers", group: "configuration" },
  { id: "routes", label: "Routes", group: "configuration" },
  { id: "keys", label: "Keys", group: "configuration" },
  { id: "settings", label: "Settings", group: "system" },
];

export const SCREEN_GROUPS: { id: ScreenGroup; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "observability", label: "Observability" },
  { id: "configuration", label: "Configuration" },
  { id: "system", label: "System" },
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
  toasts = $state<Toast[]>([]);
  confirm = $state<ConfirmRequest | null>(null);

  private healthTimer: ReturnType<typeof setInterval> | null = null;
  private toastSeq = 0;
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
}

export const app = new AppState();
