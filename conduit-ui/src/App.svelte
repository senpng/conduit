<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    LayoutDashboard,
    ChartColumn,
    Boxes,
    Route,
    KeyRound,
    Settings,
    RefreshCw,
    Waypoints,
  } from "@lucide/svelte";
  import { app, SCREENS, SCREEN_GROUPS, type ScreenId } from "./state/app.svelte";
  import { fmtMs } from "./lib/format";
  import { cn } from "./lib/utils";
  import Button from "./components/ui/button.svelte";
  import Badge from "./components/ui/badge.svelte";
  import DashboardView from "./views/DashboardView.svelte";
  import ProvidersView from "./views/ProvidersView.svelte";
  import RoutesView from "./views/RoutesView.svelte";
  import KeysView from "./views/KeysView.svelte";
  import UsageView from "./views/UsageView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import ConfirmModal from "./components/ConfirmModal.svelte";
  import Toasts from "./components/Toasts.svelte";

  const ICONS: Record<ScreenId, typeof LayoutDashboard> = {
    dashboard: LayoutDashboard,
    usage: ChartColumn,
    providers: Boxes,
    routes: Route,
    keys: KeyRound,
    settings: Settings,
  };

  onMount(() => {
    app.start();
  });

  onDestroy(() => {
    app.stop();
  });

  const current = $derived(SCREENS.find((s) => s.id === app.screen));
</script>

<div
  class="flex h-screen overflow-hidden bg-[var(--background)] text-[var(--text)] antialiased"
>
  <!-- Product sidebar: white elevated rail -->
  <aside
    class="sidebar relative z-10 flex w-[var(--sidebar-w)] shrink-0 flex-col border-r border-[var(--border)] bg-[var(--surface)] shadow-[4px_0_24px_rgb(15_23_42/0.04)]"
  >
    <div
      class="pointer-events-none absolute inset-y-0 left-0 w-1 bg-gradient-to-b from-[var(--accent)] via-[#60a5fa] to-transparent opacity-90"
      aria-hidden="true"
    ></div>

    <div class="flex items-center gap-3 border-b border-[var(--border)] px-4 py-4 pl-5">
      <div
        class="flex h-9 w-9 items-center justify-center rounded-xl bg-gradient-to-br from-[var(--accent)] to-[#3b82f6] text-white shadow-sm"
        aria-hidden="true"
      >
        <Waypoints class="h-4 w-4" />
      </div>
      <div class="min-w-0">
        <div class="text-[15px] font-semibold tracking-tight text-[var(--text)]">Conduit</div>
        <div class="text-[11px] text-[var(--text-muted)]">Local LLM gateway</div>
      </div>
      <Badge variant="secondary" class="ml-auto font-mono text-[10px]">v2</Badge>
    </div>

    <nav class="flex flex-1 flex-col gap-5 overflow-y-auto px-3 py-4">
      {#each SCREEN_GROUPS as group (group.id)}
        {@const items = SCREENS.filter((s) => s.group === group.id)}
        {#if items.length}
          <div>
            <div
              class="mb-1.5 px-2.5 text-[10px] font-semibold tracking-[0.08em] text-[var(--text-muted)] uppercase"
            >
              {group.label}
            </div>
            <div class="flex flex-col gap-0.5">
              {#each items as s (s.id)}
                {@const Icon = ICONS[s.id]}
                <button
                  type="button"
                  class={cn(
                    "nav-item group flex w-full items-center gap-2.5 rounded-xl px-2.5 py-2 text-left text-[13px] transition-all",
                    app.screen === s.id
                      ? "bg-[var(--accent-soft)] font-semibold text-[var(--accent)] shadow-sm ring-1 ring-[color-mix(in_srgb,var(--accent)_18%,transparent)]"
                      : "text-[var(--text-secondary)] hover:bg-[var(--surface-muted)] hover:text-[var(--text)]",
                  )}
                  onclick={() => app.goto(s.id)}
                >
                  <span
                    class={cn(
                      "flex h-7 w-7 items-center justify-center rounded-lg transition-colors",
                      app.screen === s.id
                        ? "bg-white text-[var(--accent)] shadow-sm"
                        : "bg-transparent text-[var(--text-muted)] group-hover:text-[var(--text)]",
                    )}
                  >
                    <Icon class="h-4 w-4" />
                  </span>
                  <span class="truncate">{s.label}</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}
      {/each}
    </nav>

    <div class="border-t border-[var(--border)] bg-[var(--surface-muted)]/40 px-4 py-3.5">
      <div class="flex items-center gap-2 text-xs font-medium text-[var(--text-secondary)]">
        <span
          class={cn(
            "h-2 w-2 shrink-0 rounded-full",
            app.healthError
              ? "bg-[var(--danger)] ring-4 ring-[var(--danger-soft)]"
              : "bg-[var(--success)] ring-4 ring-[var(--success-soft)]",
          )}
        ></span>
        <span>
          {app.healthError ? "Daemon offline" : "Daemon online"}
          {#if app.health?.version}
            <span class="font-normal text-[var(--text-muted)]">· v{app.health.version}</span>
          {/if}
        </span>
      </div>
      <div class="mono mt-1.5 text-[10.5px] leading-relaxed text-[var(--text-muted)]">
        {app.consoleBase.replace(/^https?:\/\//, "")}
        {#if app.rttMs != null}
          · {fmtMs(app.rttMs)}
        {/if}
      </div>
    </div>
  </aside>

  <main class="flex min-w-0 flex-1 flex-col">
    <header
      class="flex h-[var(--topbar-h)] shrink-0 items-center gap-3 border-b border-[var(--border)] bg-[var(--surface)]/90 px-6 backdrop-blur-sm"
    >
      <div>
        <div class="page-title text-lg font-semibold tracking-tight text-[var(--text)]">
          {current?.label ?? ""}
        </div>
      </div>
      <Badge variant={app.isLoopback ? "success" : "warning"} class="font-mono text-[10px]">
        {app.isLoopback ? "loopback" : "remote"}
      </Badge>
      <div class="ml-auto flex items-center gap-2">
        <Button
          variant="ghost"
          size="icon"
          title="Refresh"
          onclick={() => app.refreshCurrent()}
        >
          <RefreshCw class="h-4 w-4" />
        </Button>
      </div>
    </header>

    <div class="view-scroll">
      {#if app.screen === "dashboard"}
        <DashboardView />
      {:else if app.screen === "usage"}
        <UsageView />
      {:else if app.screen === "providers"}
        <ProvidersView />
      {:else if app.screen === "routes"}
        <RoutesView />
      {:else if app.screen === "keys"}
        <KeysView />
      {:else if app.screen === "settings"}
        <SettingsView />
      {/if}
    </div>
  </main>
</div>

<ConfirmModal />
<Toasts />
