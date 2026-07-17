<script lang="ts">
  import { app } from "../state/app.svelte";
  import {
    routes as routesApi,
    providers as providersApi,
  } from "../lib/adminClient";
  import type { Provider, Route } from "../lib/adminClient";
  import Modal from "../components/Modal.svelte";

  interface Props {
    /** Existing route → edit mode (PUT); absent → create (POST). */
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
  }

  interface ExistingTarget {
    provider_id?: string;
    provider_kind?: string;
    model_id?: string;
    upstream_key_id?: string;
    base_url?: string;
  }

  let providers = $state<Provider[]>([]);
  let alias = $state("");
  let strategy = $state("fixed");
  let targets = $state<TargetDraft[]>([]);
  let retryJson = $state("");
  let formError = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    // Prefill once on mount (component is remounted per open by the parent).
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
      { provider_id: "", provider_kind: "", model_id: "", upstream_key_id: "", base_url: "" },
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
          ...(t.base_url.trim() ? { base_url: t.base_url.trim() } : {}),
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

<Modal wide onclose={() => ondone(false)}>
  <h3>{existing ? `Edit route — ${existing.match_alias}` : "New route"}</h3>

  <div class="form-grid">
    <label>
      Match alias
      <input bind:value={alias} placeholder="gpt-4o" />
      <span class="field-hint">The model name downstream clients send.</span>
    </label>
    <label>
      Strategy
      <select bind:value={strategy}>
        <option value="fixed">Fixed — always target #0</option>
        <option value="fallback">Fallback — try in order</option>
      </select>
    </label>
  </div>

  <div>
    <div class="row-between" style="margin-bottom:8px">
      <span class="panel-title">
        Targets {#if strategy === "fallback"}(attempt order){/if}
      </span>
      <button class="btn-ghost btn-sm" onclick={addTarget}>＋ Add target</button>
    </div>
    <div class="target-list">
      {#each targets as t, i (i)}
        <div class="target-row">
          <span class="target-order">#{i}</span>
          <select bind:value={t.provider_id} onchange={() => onProviderPick(t)}>
            <option value="" disabled>Provider…</option>
            {#each providers as p (p.id)}
              <option value={p.id}>{p.name} ({p.kind})</option>
            {/each}
          </select>
          <input bind:value={t.model_id} placeholder="model_id e.g. gpt-4o" />
          <input
            bind:value={t.upstream_key_id}
            placeholder="upstream_key_id"
            title="Secret scope binding; defaults to provider id"
          />
          <div class="target-actions">
            <button class="btn-icon" title="Move up" disabled={i === 0} onclick={() => move(i, -1)}>↑</button>
            <button class="btn-icon" title="Move down" disabled={i === targets.length - 1} onclick={() => move(i, 1)}>↓</button>
            <button class="btn-icon danger" title="Remove" onclick={() => removeTarget(i)}>✕</button>
          </div>
        </div>
      {/each}
    </div>
  </div>

  <label>
    Retry policy (optional JSON)
    <textarea rows="2" bind:value={retryJson} placeholder={'{"max_retries":2,"base_delay_ms":500}'}></textarea>
  </label>

  {#if formError}
    <div class="form-error">{formError}</div>
  {/if}

  <div class="form-actions">
    <button class="btn-primary" disabled={busy} onclick={submit}>
      {busy ? "Saving…" : existing ? "Save changes" : "Create route"}
    </button>
    <button class="btn-ghost" onclick={() => ondone(false)}>Cancel</button>
  </div>
</Modal>
