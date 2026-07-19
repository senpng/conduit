<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../state/app.svelte";
  import { pricing as pricingApi } from "../lib/consoleClient";
  import type { PricingRow } from "../lib/consoleClient";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import Spinner from "../components/app/Spinner.svelte";
  import Button from "../components/ui/button.svelte";
  import Badge from "../components/ui/badge.svelte";
  import {
    tableWrapClass,
    tableClass,
    thClass,
    tdClass,
    monoClass,
    dimClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";
  import { Download, RefreshCw } from "@lucide/svelte";

  let list = $state<PricingRow[]>([]);
  let loading = $state(true);
  let reloading = $state(false);
  let syncing = $state(false);

  async function load() {
    loading = true;
    try {
      list = await pricingApi.list();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  async function reload() {
    reloading = true;
    try {
      await pricingApi.reload();
      app.toast("pricing layers reloaded", "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      reloading = false;
    }
  }

  async function syncLiteLlm() {
    syncing = true;
    try {
      const r = await pricingApi.sync();
      const n = r.source_models ?? 0;
      const total = r.total_rows ?? list.length;
      app.toast(`LiteLLM synced · ${n} models → ${total} rows`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      syncing = false;
    }
  }

  onMount(() => {
    void load();
  });

  const money = (v: number | null) => (v != null ? `$${v.toFixed(2)}` : "—");
</script>

<Card>
  <PageHeader
    title="Model pricing"
    description="Layers: embedded defaults → pricing.litellm.json → pricing.json (overrides win). Sync pulls the LiteLLM cost map (chat/completion only)."
  >
    {#snippet actions()}
      <Button variant="outline" size="sm" disabled={syncing || reloading} onclick={syncLiteLlm}>
        <Download class="h-3.5 w-3.5" />
        {syncing ? "Syncing…" : "Sync LiteLLM"}
      </Button>
      <Button variant="outline" size="sm" disabled={reloading || syncing} onclick={reload}>
        <RefreshCw class="h-3.5 w-3.5" />
        {reloading ? "Reloading…" : "Reload layers"}
      </Button>
    {/snippet}
  </PageHeader>

  {#if loading && list.length === 0}
    <Spinner />
  {:else}
    <div class={tableWrapClass}>
      <table class={tableClass}>
        <thead>
          <tr>
            <th class={thClass}>Provider</th>
            <th class={thClass}>Model</th>
            <th class={thClass}>Input/Mtok</th>
            <th class={thClass}>Output/Mtok</th>
            <th class={thClass}>Cache read</th>
            <th class={thClass}>Cache write</th>
            <th class={thClass}>Reasoning</th>
            <th class={thClass}>Effective from</th>
          </tr>
        </thead>
        <tbody>
          {#each list as p (p.provider_kind + p.model_id + p.effective_from)}
            <tr class="hover:bg-[var(--surface-muted)]/80">
              <td class={tdClass}><Badge variant="secondary">{p.provider_kind}</Badge></td>
              <td class={cn(tdClass, monoClass, "text-xs")}>{p.model_id}</td>
              <td class={cn(tdClass, monoClass)}>{money(p.input_per_mtok)}</td>
              <td class={cn(tdClass, monoClass)}>{money(p.output_per_mtok)}</td>
              <td class={cn(tdClass, monoClass, dimClass)}>{money(p.cache_read_per_mtok)}</td>
              <td class={cn(tdClass, monoClass, dimClass)}>{money(p.cache_write_per_mtok)}</td>
              <td class={cn(tdClass, monoClass, dimClass)}>{money(p.reasoning_per_mtok)}</td>
              <td class={cn(tdClass, dimClass, "text-xs")}>{p.effective_from}</td>
            </tr>
          {:else}
            <tr>
              <td class={cn(tdClass, "py-8 text-center text-[var(--text-muted)]")} colspan="8">
                No pricing data loaded.
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</Card>
