<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app, SCREENS } from "./state/app.svelte";
  import { fmtMs } from "./lib/format";
  import DashboardView from "./views/DashboardView.svelte";
  import LiveView from "./views/LiveView.svelte";
  import TracesView from "./views/TracesView.svelte";
  import ProvidersView from "./views/ProvidersView.svelte";
  import RoutesView from "./views/RoutesView.svelte";
  import KeysView from "./views/KeysView.svelte";
  import UsageView from "./views/UsageView.svelte";
  import PricingView from "./views/PricingView.svelte";
  import Palette from "./components/Palette.svelte";
  import HelpOverlay from "./components/HelpOverlay.svelte";
  import ConfirmModal from "./components/ConfirmModal.svelte";
  import Toasts from "./components/Toasts.svelte";

  onMount(() => {
    app.start();
    window.addEventListener("keydown", app.handleKey);
  });

  onDestroy(() => {
    app.stop();
    window.removeEventListener("keydown", app.handleKey);
  });

  const current = $derived(SCREENS.find((s) => s.id === app.screen));
</script>

<div class="shell">
  <aside class="sidebar">
    <div class="brand">
      <span class="logo">⊃</span>
      <span class="brand-name">Conduit</span>
      <span class="brand-tag">v2</span>
    </div>

    <nav class="nav">
      {#each SCREENS as s (s.id)}
        <button
          class="nav-item"
          class:active={app.screen === s.id}
          onclick={() => app.goto(s.id)}
        >
          <span class="nav-icon">{s.icon}</span>
          <span>{s.label}</span>
          <span class="nav-key">g {s.g}</span>
        </button>
      {/each}
    </nav>

    <div class="sidebar-footer">
      <div
        class="health-indicator"
        class:healthy={!app.healthError}
        class:unhealthy={app.healthError}
      >
        <span class="health-dot"></span>
        <span>
          {app.healthError ? "Daemon offline" : "Online"}
          {#if app.health?.version}
            <span class="muted">v{app.health.version}</span>
          {/if}
        </span>
      </div>
      <div class="health-meta">
        {app.consoleBase.replace(/^https?:\/\//, "")}
        {#if app.rttMs != null}
          · {fmtMs(app.rttMs)}
        {/if}
        {#if app.health?.trace_enabled != null}
          · trace {app.health.trace_enabled ? "on" : "off"}
        {/if}
      </div>
    </div>
  </aside>

  <main class="content">
    <header class="topbar">
      <div class="page-title">{current?.label ?? ""}</div>
      <div class="topbar-sub mono">{app.isLoopback ? "loopback" : "remote"}</div>
      <div class="topbar-actions">
        <button class="btn-ghost btn-sm" onclick={() => (app.paletteOpen = true)}>
          ⌘K palette
        </button>
        <button class="btn-ghost btn-sm" title="Refresh (r)" onclick={() => app.refreshCurrent()}>
          ↺
        </button>
      </div>
    </header>

    <div class="view-scroll">
      {#if app.screen === "dashboard"}
        <DashboardView />
      {:else if app.screen === "live"}
        <LiveView />
      {:else if app.screen === "traces"}
        <TracesView />
      {:else if app.screen === "providers"}
        <ProvidersView />
      {:else if app.screen === "routes"}
        <RoutesView />
      {:else if app.screen === "keys"}
        <KeysView />
      {:else if app.screen === "usage"}
        <UsageView />
      {:else if app.screen === "pricing"}
        <PricingView />
      {/if}
    </div>
  </main>
</div>

<Palette />
<HelpOverlay />
<ConfirmModal />
<Toasts />
