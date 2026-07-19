<script lang="ts">
  import { app } from "../state/app.svelte";
  import { fmtMs } from "../lib/format";
  import Badge from "../components/ui/badge.svelte";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import { monoClass } from "$lib/ui";
  import { cn } from "$lib/utils";
</script>

<div class="mx-auto flex max-w-2xl flex-col gap-4">
  <Card>
    <PageHeader
      title="Connection"
      description="Console API base (build-time / env). Read-only in the UI."
    />
    <dl class="grid gap-3 text-sm">
      <div class="flex items-center justify-between gap-3">
        <dt class="text-[var(--text-secondary)]">Endpoint</dt>
        <dd class={cn(monoClass, "text-[var(--text)]")}>{app.consoleBase}</dd>
      </div>
      <div class="flex items-center justify-between gap-3">
        <dt class="text-[var(--text-secondary)]">Network</dt>
        <dd>
          {#if app.isLoopback}
            <Badge variant="success">loopback</Badge>
          {:else}
            <Badge variant="warning">remote</Badge>
          {/if}
        </dd>
      </div>
      <div class="flex items-center justify-between gap-3">
        <dt class="text-[var(--text-secondary)]">Daemon</dt>
        <dd class="flex items-center gap-2">
          {#if app.healthError}
            <Badge variant="danger">offline</Badge>
          {:else}
            <Badge variant="success">online</Badge>
            {#if app.health?.version}
              <span class={cn(monoClass, "text-xs text-[var(--text-muted)]")}>
                v{app.health.version}
              </span>
            {/if}
          {/if}
        </dd>
      </div>
      <div class="flex items-center justify-between gap-3">
        <dt class="text-[var(--text-secondary)]">RTT</dt>
        <dd class={cn(monoClass, "text-[var(--text)]")}>
          {app.rttMs != null ? fmtMs(app.rttMs) : "—"}
        </dd>
      </div>
    </dl>
  </Card>

  <Card>
    <PageHeader title="About" />
    <p class="text-sm text-[var(--text-secondary)]">
      Conduit operator console — local-first gateway control surface. Data stays on the host
      running <span class={monoClass}>conduitd</span>. UI chrome uses system fonts only (no
      remote assets). Light product theme only.
    </p>
  </Card>
</div>
