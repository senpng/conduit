<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { keys as keysApi } from "../lib/consoleClient";
  import type { DownstreamKey, CreateKeyResponse } from "../lib/consoleClient";
  import { fmtDate } from "../lib/format";
  import KeyCreatedModal from "../components/KeyCreatedModal.svelte";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import Spinner from "../components/app/Spinner.svelte";
  import Field from "../components/app/Field.svelte";
  import Button from "../components/ui/button.svelte";
  import Badge from "../components/ui/badge.svelte";
  import {
    controlClass,
    tableWrapClass,
    tableClass,
    thClass,
    tdClass,
    monoClass,
    dimClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";
  import { Plus, Trash2 } from "@lucide/svelte";

  let list = $state<DownstreamKey[]>([]);
  let loading = $state(true);
  let showForm = $state(false);
  let reveal = $state<CreateKeyResponse | null>(null);
  let form = $state({ name: "", rate_limit_rpm: "" });

  async function load() {
    loading = true;
    try {
      list = await keysApi.list();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("keys", () => void load());
  });
  onDestroy(() => unregister?.());

  async function create() {
    if (!form.name.trim()) {
      app.toast("Name is required", "warn");
      return;
    }
    try {
      const result = await keysApi.create({
        name: form.name.trim(),
        rate_limit_rpm: form.rate_limit_rpm ? Number(form.rate_limit_rpm) : undefined,
      });
      reveal = result;
      showForm = false;
      form = { name: "", rate_limit_rpm: "" };
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function toggleEnabled(k: DownstreamKey) {
    try {
      await keysApi.update(k.id, { enabled: !k.enabled });
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function remove(k: DownstreamKey) {
    const ok = await app.askConfirm({
      title: `Revoke key "${k.name}"?`,
      body: `id ${k.id}. Clients using this key will lose access immediately. This cannot be undone.`,
      confirmLabel: "Revoke",
    });
    if (!ok) return;
    try {
      await keysApi.delete(k.id);
      app.toast(`Key "${k.name}" revoked`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }
</script>

<Card>
  <PageHeader title="Downstream keys" description="Client credentials for the gateway.">
    {#snippet actions()}
      <Button onclick={() => (showForm = !showForm)}>
        <Plus class="h-4 w-4" />
        {showForm ? "Close form" : "Create key"}
      </Button>
    {/snippet}
  </PageHeader>

  {#if showForm}
    <div class="mb-4 rounded-lg border border-[var(--border)] bg-[var(--surface-muted)]/60 p-4">
      <h3 class="mb-3 text-sm font-semibold text-[var(--text)]">New downstream key</h3>
      <div class="grid gap-3 sm:grid-cols-2">
        <Field label="Name">
          <input class={controlClass} bind:value={form.name} placeholder="My app" />
        </Field>
        <Field label="Rate limit (RPM)">
          <input
            class={controlClass}
            type="number"
            bind:value={form.rate_limit_rpm}
            placeholder="60"
            min="0"
          />
        </Field>
      </div>
      <div class="mt-3 flex gap-2">
        <Button onclick={create}>Create</Button>
        <Button variant="outline" onclick={() => (showForm = false)}>Cancel</Button>
      </div>
    </div>
  {/if}

  {#if loading && list.length === 0}
    <Spinner />
  {:else}
    <div class={tableWrapClass}>
      <table class={tableClass}>
        <thead>
          <tr>
            <th class={thClass}>Name</th>
            <th class={thClass}>RPM</th>
            <th class={thClass}>Status</th>
            <th class={thClass}>Created</th>
            <th class={thClass}></th>
          </tr>
        </thead>
        <tbody>
          {#each list as k (k.id)}
            <tr class="hover:bg-[var(--surface-muted)]/80">
              <td class={tdClass}>{k.name}</td>
              <td class={cn(tdClass, monoClass)}>{k.rate_limit_rpm ?? "—"}</td>
              <td class={tdClass}>
                <button
                  type="button"
                  class="cursor-pointer border-0 bg-transparent p-0"
                  title="Toggle enabled"
                  onclick={() => void toggleEnabled(k)}
                >
                  <Badge variant={k.enabled ? "success" : "secondary"}>
                    {k.enabled ? "active" : "disabled"}
                  </Badge>
                </button>
              </td>
              <td class={cn(tdClass, dimClass, "text-xs")}>{fmtDate(k.created_at)}</td>
              <td class={cn(tdClass, "w-px")}>
                <Button
                  variant="ghost"
                  size="icon"
                  title="Revoke"
                  onclick={() => void remove(k)}
                >
                  <Trash2 class="h-4 w-4 text-[var(--danger)]" />
                </Button>
              </td>
            </tr>
          {:else}
            <tr>
              <td class={cn(tdClass, "py-8 text-center text-[var(--text-muted)]")} colspan="5">
                No keys yet.
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</Card>

{#if reveal}
  <KeyCreatedModal keyData={reveal} onclose={() => (reveal = null)} />
{/if}
