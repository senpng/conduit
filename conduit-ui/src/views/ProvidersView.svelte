<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { providers as providersApi } from "../lib/consoleClient";
  import type { Provider } from "../lib/consoleClient";
  import { fmtDate } from "../lib/format";
  import Modal from "../components/Modal.svelte";
  import OAuthPanel from "./OAuthPanel.svelte";
  import Card from "../components/app/Card.svelte";
  import PageHeader from "../components/app/PageHeader.svelte";
  import Spinner from "../components/app/Spinner.svelte";
  import Field from "../components/app/Field.svelte";
  import Button from "../components/ui/button.svelte";
  import Badge from "../components/ui/badge.svelte";
  import {
    controlClass,
    selectClass,
    tableWrapClass,
    tableClass,
    thClass,
    tdClass,
    monoClass,
    dimClass,
    mutedClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";
  import { Plus, Pencil, KeyRound, Trash2 } from "@lucide/svelte";

  let list = $state<Provider[]>([]);
  let loading = $state(true);
  let showForm = $state(false);
  let showOAuth = $state(false);
  let secretFor = $state<Provider | null>(null);
  let secretValue = $state("");
  let editing = $state<Provider | null>(null);
  let editForm = $state({ name: "", base_url: "" });

  let form = $state({
    name: "",
    kind: "openai",
    base_url: "https://api.openai.com",
    api_key: "",
  });

  const kindDefaults: Record<string, string> = {
    openai: "https://api.openai.com",
    anthropic: "https://api.anthropic.com",
    "claude-oauth": "https://api.anthropic.com",
    "codex-oauth": "https://chatgpt.com/backend-api/codex",
    "grok-oauth": "https://cli-chat-proxy.grok.com/v1",
    other: "",
  };

  async function load() {
    loading = true;
    try {
      list = await providersApi.list();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      loading = false;
    }
  }

  let unregister: (() => void) | null = null;
  onMount(() => {
    void load();
    unregister = app.registerRefresher("providers", () => void load());
  });
  onDestroy(() => unregister?.());

  function onKindChange() {
    const d = kindDefaults[form.kind];
    if (d !== undefined) form.base_url = d;
  }

  async function create() {
    if (!form.name.trim()) {
      app.toast("Name is required", "warn");
      return;
    }
    try {
      await providersApi.create({
        name: form.name.trim(),
        kind: form.kind,
        base_url: form.base_url.trim(),
        api_key: form.api_key || undefined,
      });
      showForm = false;
      form = { name: "", kind: "openai", base_url: kindDefaults.openai, api_key: "" };
      app.toast("Provider created", "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function remove(p: Provider) {
    const ok = await app.askConfirm({
      title: `Delete provider "${p.name}"?`,
      body: `id ${p.id} · kind ${p.kind}. This cannot be undone.`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await providersApi.delete(p.id);
      app.toast(`Provider "${p.name}" deleted`, "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function saveSecret() {
    if (!secretFor || !secretValue.trim()) return;
    const p = secretFor;
    const ok = await app.askConfirm({
      title: `Rotate upstream secret for "${p.name}"?`,
      body: `The stored credential for id ${p.id} will be overwritten.`,
      confirmLabel: "Overwrite",
      danger: true,
    });
    if (!ok) return;
    try {
      await providersApi.setSecret(p.id, secretValue.trim());
      secretFor = null;
      secretValue = "";
      app.toast("Secret updated", "ok");
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  function startEdit(p: Provider) {
    editing = p;
    editForm = { name: p.name, base_url: p.base_url };
  }

  async function saveEdit() {
    if (!editing) return;
    try {
      await providersApi.update(editing.id, {
        name: editForm.name.trim() || undefined,
        base_url: editForm.base_url.trim() || undefined,
      });
      editing = null;
      app.toast("Provider updated", "ok");
      await load();
    } catch (e: unknown) {
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }
</script>

<Card>
  <PageHeader title="Upstream providers" description="API-key and OAuth upstream accounts.">
    {#snippet actions()}
      <Button variant="outline" onclick={() => (showOAuth = true)}>OAuth login</Button>
      <Button onclick={() => (showForm = !showForm)}>
        <Plus class="h-4 w-4" />
        {showForm ? "Close form" : "Add provider"}
      </Button>
    {/snippet}
  </PageHeader>

  {#if showForm}
    <div class="mb-4 rounded-lg border border-[var(--border)] bg-[var(--surface-muted)]/60 p-4">
      <h3 class="mb-3 text-sm font-semibold text-[var(--text)]">New provider (API key)</h3>
      <div class="grid gap-3 sm:grid-cols-2">
        <Field label="Name">
          <input class={controlClass} bind:value={form.name} placeholder="My OpenAI account" />
        </Field>
        <Field label="Kind">
          <select class={selectClass} bind:value={form.kind} onchange={onKindChange}>
            <option value="openai">OpenAI</option>
            <option value="anthropic">Anthropic</option>
            <option value="other">Other (OpenAI-compatible)</option>
          </select>
        </Field>
        <Field label="Base URL">
          <input class={controlClass} bind:value={form.base_url} placeholder="https://api.openai.com" />
        </Field>
        <Field label="API key (optional)" hint="Stored in OS keychain / master-password store.">
          <input
            class={controlClass}
            type="password"
            bind:value={form.api_key}
            placeholder="sk-…"
            autocomplete="off"
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
            <th class={thClass}>Kind</th>
            <th class={thClass}>Base URL</th>
            <th class={thClass}>Key ref</th>
            <th class={thClass}>Created</th>
            <th class={thClass}></th>
          </tr>
        </thead>
        <tbody>
          {#each list as p (p.id)}
            <tr class="hover:bg-[var(--surface-muted)]/80">
              <td class={tdClass}>{p.name}</td>
              <td class={tdClass}><Badge>{p.kind}</Badge></td>
              <td class={cn(tdClass, monoClass, dimClass, "text-xs")}>{p.base_url}</td>
              <td class={cn(tdClass, monoClass, mutedClass, "text-[11px]")}>{p.upstream_key_ref}</td>
              <td class={cn(tdClass, dimClass, "text-xs")}>{fmtDate(p.created_at)}</td>
              <td class={cn(tdClass, "w-px")}>
                <div class="flex items-center gap-0.5">
                  <Button variant="ghost" size="icon" title="Edit" onclick={() => startEdit(p)}>
                    <Pencil class="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    title="Rotate API key"
                    onclick={() => {
                      secretFor = p;
                      secretValue = "";
                    }}
                  >
                    <KeyRound class="h-4 w-4" />
                  </Button>
                  <Button variant="ghost" size="icon" title="Delete" onclick={() => void remove(p)}>
                    <Trash2 class="h-4 w-4 text-[var(--danger)]" />
                  </Button>
                </div>
              </td>
            </tr>
          {:else}
            <tr>
              <td class={cn(tdClass, "py-8 text-center text-[var(--text-muted)]")} colspan="6">
                No providers yet — add one or use OAuth login.
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</Card>

{#if showOAuth}
  <OAuthPanel
    ondone={() => {
      showOAuth = false;
      void load();
    }}
  />
{/if}

{#if secretFor}
  <Modal onclose={() => (secretFor = null)} title={`Rotate API key — ${secretFor.name}`}>
    <p class="mb-3 text-sm text-[var(--text-secondary)]">
      Stored via the secret backend. Overwrite requires confirmation.
    </p>
    <input
      class={cn(controlClass, "mb-4")}
      type="password"
      bind:value={secretValue}
      placeholder="sk-…"
      autocomplete="off"
    />
    <div class="flex justify-end gap-2">
      <Button variant="destructive" disabled={!secretValue.trim()} onclick={saveSecret}>
        Overwrite secret
      </Button>
      <Button variant="outline" onclick={() => (secretFor = null)}>Cancel</Button>
    </div>
  </Modal>
{/if}

{#if editing}
  <Modal onclose={() => (editing = null)} title={`Edit provider — ${editing.name}`}>
    <div class="mb-4 grid gap-3">
      <Field label="Name">
        <input class={controlClass} bind:value={editForm.name} />
      </Field>
      <Field label="Base URL">
        <input class={controlClass} bind:value={editForm.base_url} />
      </Field>
    </div>
    <div class="flex justify-end gap-2">
      <Button onclick={saveEdit}>Save</Button>
      <Button variant="outline" onclick={() => (editing = null)}>Cancel</Button>
    </div>
  </Modal>
{/if}
