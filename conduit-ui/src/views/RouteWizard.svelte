<script lang="ts">
  import { app } from "../state/app.svelte";
  import {
    routes as routesApi,
    providers as providersApi,
  } from "../lib/consoleClient";
  import type { Provider, Route } from "../lib/consoleClient";
  import Modal from "../components/Modal.svelte";
  import Field from "../components/app/Field.svelte";
  import Button from "../components/ui/button.svelte";
  import {
    controlClass,
    selectClass,
    textareaClass,
    monoClass,
  } from "$lib/ui";
  import { cn } from "$lib/utils";
  import { Plus, ChevronUp, ChevronDown, Trash2 } from "@lucide/svelte";

  interface Props {
    existing?: Route | null;
    ondone: (changed: boolean) => void;
  }
  let { existing = null, ondone }: Props = $props();

  interface TargetDraft {
    provider_id: string;
    provider_kind: string;
    model_id: string;
    upstream_key_id: string;
    base_url: string;
    weight: number;
    request_overrides: string;
  }

  interface ExistingTarget {
    provider_id?: string;
    provider_kind?: string;
    model_id?: string;
    upstream_key_id?: string;
    base_url?: string;
    weight?: number;
    request_overrides?: Record<string, unknown>;
  }

  let providers = $state<Provider[]>([]);
  let alias = $state("");
  let strategy = $state("fixed");
  let targets = $state<TargetDraft[]>([]);
  let retryJson = $state("");
  let formError = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    alias = existing?.match_alias ?? "";
    strategy = existing?.strategy ?? "fixed";
    void (async () => {
      try {
        providers = await providersApi.list();
      } catch (e: unknown) {
        app.toast(e instanceof Error ? e.message : String(e), "error");
      }
      if (existing) {
        try {
          const parsed = JSON.parse(existing.targets_json) as ExistingTarget[];
          targets = parsed.map((t) => ({
            provider_id: t.provider_id ?? "",
            provider_kind: t.provider_kind ?? "",
            model_id: t.model_id ?? "",
            upstream_key_id: t.upstream_key_id ?? "",
            base_url: t.base_url ?? "",
            weight: typeof t.weight === "number" && t.weight >= 0 ? t.weight : 1,
            request_overrides: t.request_overrides
              ? JSON.stringify(t.request_overrides)
              : "",
          }));
        } catch {
          app.toast("Existing targets_json is not valid JSON — starting empty", "warn");
        }
        retryJson =
          existing.retry_policy_json && existing.retry_policy_json !== "null"
            ? existing.retry_policy_json
            : "";
      }
      if (targets.length === 0) addTarget();
    })();
  });

  function addTarget() {
    targets = [
      ...targets,
      {
        provider_id: "",
        provider_kind: "",
        model_id: "",
        upstream_key_id: "",
        base_url: "",
        weight: 1,
        request_overrides: "",
      },
    ];
  }

  function removeTarget(i: number) {
    targets = targets.filter((_, j) => j !== i);
  }

  function move(i: number, dir: -1 | 1) {
    const j = i + dir;
    if (j < 0 || j >= targets.length) return;
    const next = [...targets];
    [next[i], next[j]] = [next[j], next[i]];
    targets = next;
  }

  function onProviderPick(t: TargetDraft) {
    const p = providers.find((x) => x.id === t.provider_id);
    if (p) {
      t.provider_kind = p.kind;
      if (!t.upstream_key_id) t.upstream_key_id = p.id;
    }
  }

  function validate(): string | null {
    if (!alias.trim()) return "Alias is required";
    if (targets.length === 0) return "At least one target is required";
    for (const [i, t] of targets.entries()) {
      if (!t.provider_id) return `Target #${i}: provider is required`;
      if (!t.model_id.trim()) return `Target #${i}: model_id is required`;
      if (!t.upstream_key_id.trim()) return `Target #${i}: upstream_key_id is required`;
      if (t.request_overrides.trim()) {
        try {
          const overrides = JSON.parse(t.request_overrides);
          if (!overrides || Array.isArray(overrides) || typeof overrides !== "object") {
            return `Target #${i}: request overrides must be a JSON object`;
          }
        } catch {
          return `Target #${i}: request overrides must be valid JSON`;
        }
      }
    }
    if (retryJson.trim()) {
      try {
        JSON.parse(retryJson);
      } catch {
        return "Retry policy must be valid JSON";
      }
    }
    return null;
  }

  async function submit() {
    formError = validate();
    if (formError) return;
    if (strategy === "fixed" && targets.length > 1) {
      app.toast("Fixed strategy only uses target #0; extras kept for reference", "warn");
    }
    if ((strategy === "fallback" || strategy === "weighted") && targets.length < 2) {
      app.toast("Multi-target strategies with one target behave like fixed", "warn");
    }
    busy = true;
    try {
      const body = {
        match_alias: alias.trim(),
        strategy,
        targets: targets.map((t) => ({
          provider_id: t.provider_id,
          provider_kind: t.provider_kind,
          model_id: t.model_id.trim(),
          upstream_key_id: t.upstream_key_id.trim(),
          weight: Math.max(0, Math.floor(Number(t.weight) || 0)),
          ...(t.base_url.trim() ? { base_url: t.base_url.trim() } : {}),
          ...(t.request_overrides.trim()
            ? { request_overrides: JSON.parse(t.request_overrides) }
            : {}),
        })),
        retry_policy: retryJson.trim() ? JSON.parse(retryJson) : undefined,
      };
      if (existing) {
        await routesApi.update(existing.id, body);
        app.toast(`Route "${body.match_alias}" updated`, "ok");
      } else {
        await routesApi.create(body);
        app.toast(`Route "${body.match_alias}" created`, "ok");
      }
      ondone(true);
    } catch (e: unknown) {
      formError = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal
  wide
  onclose={() => ondone(false)}
  title={existing ? `Edit route — ${existing.match_alias}` : "New route"}
>
  <div class="mb-4 grid gap-3 sm:grid-cols-2">
    <Field label="Match alias" hint="The model name downstream clients send.">
      <input class={controlClass} bind:value={alias} placeholder="gpt-4o" />
    </Field>
    <Field label="Strategy">
      <select class={selectClass} bind:value={strategy}>
        <option value="fixed">Fixed — always target #0</option>
        <option value="fallback">Fallback — ordered failover + sticky</option>
        <option value="weighted">Weighted — LB by weight + sticky</option>
      </select>
      {#if strategy === "fallback"}
        <span class="mt-1 text-[11px] font-normal text-[var(--text-muted)]">
          Sticky: last successful provider per key + alias is tried first.
        </span>
      {:else if strategy === "weighted"}
        <span class="mt-1 text-[11px] font-normal text-[var(--text-muted)]">
          Attempt 0 picks by relative weight; sticky pin overrides when present.
        </span>
      {/if}
    </Field>
  </div>

  <div class="mb-4">
    <div class="mb-2 flex items-center justify-between gap-2">
      <span class="text-sm font-semibold text-[var(--text)]">
        Targets
        {#if strategy === "fallback"}
          <span class="font-normal text-[var(--text-muted)]">(order; sticky first)</span>
        {:else if strategy === "weighted"}
          <span class="font-normal text-[var(--text-muted)]">(weights + sticky)</span>
        {/if}
      </span>
      <Button variant="outline" size="sm" onclick={addTarget}>
        <Plus class="h-3.5 w-3.5" />
        Add target
      </Button>
    </div>
    <div class="space-y-2">
      {#each targets as t, i (i)}
        <div class="rounded-lg border border-[var(--border)] bg-[var(--surface-muted)]/40 p-3">
          <div class="mb-2 flex flex-wrap items-center gap-2">
            <span class={cn(monoClass, "text-xs text-[var(--text-muted)]")}>#{i}</span>
            <select
              class={cn(selectClass, "min-w-[10rem] flex-1")}
              bind:value={t.provider_id}
              onchange={() => onProviderPick(t)}
            >
              <option value="" disabled>Provider…</option>
              {#each providers as p (p.id)}
                <option value={p.id}>{p.name} ({p.kind})</option>
              {/each}
            </select>
            <input
              class={cn(controlClass, "min-w-[8rem] flex-1")}
              bind:value={t.model_id}
              placeholder="model_id e.g. gpt-4o"
            />
            <input
              class={cn(controlClass, "min-w-[8rem] flex-1")}
              bind:value={t.upstream_key_id}
              placeholder="upstream_key_id"
              title="Secret scope binding; defaults to provider id"
            />
            {#if strategy === "weighted"}
              <input
                class={cn(controlClass, "w-20")}
                type="number"
                min="0"
                step="1"
                bind:value={t.weight}
                title="Relative weight"
                placeholder="weight"
              />
            {/if}
            <div class="flex gap-0.5">
              <Button variant="ghost" size="icon" title="Move up" disabled={i === 0} onclick={() => move(i, -1)}>
                <ChevronUp class="h-4 w-4" />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                title="Move down"
                disabled={i === targets.length - 1}
                onclick={() => move(i, 1)}
              >
                <ChevronDown class="h-4 w-4" />
              </Button>
              <Button variant="ghost" size="icon" title="Remove" onclick={() => removeTarget(i)}>
                <Trash2 class="h-4 w-4 text-[var(--danger)]" />
              </Button>
            </div>
          </div>
          <Field label="Request overrides (JSON)">
            <input
              class={controlClass}
              bind:value={t.request_overrides}
              placeholder={'{"service_tier":"priority"}'}
            />
          </Field>
        </div>
      {/each}
    </div>
  </div>

  <Field label="Retry policy (optional JSON)" class="mb-4">
    <textarea
      class={textareaClass}
      rows="2"
      bind:value={retryJson}
      placeholder={'{"max_retries":2,"base_delay_ms":500}'}
    ></textarea>
  </Field>

  {#if formError}
    <div
      class="mb-3 rounded-lg border border-[color-mix(in_srgb,var(--danger)_30%,var(--border))] bg-[var(--danger-soft)] px-3 py-2 text-sm text-[var(--danger)]"
    >
      {formError}
    </div>
  {/if}

  <div class="flex justify-end gap-2">
    <Button disabled={busy} onclick={submit}>
      {busy ? "Saving…" : existing ? "Save changes" : "Create route"}
    </Button>
    <Button variant="outline" onclick={() => ondone(false)}>Cancel</Button>
  </div>
</Modal>
