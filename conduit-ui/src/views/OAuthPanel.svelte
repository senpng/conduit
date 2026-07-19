<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { oauth as oauthApi, providers as providersApi } from "../lib/consoleClient";
  import type { OAuthProviderMeta, OAuthSession } from "../lib/consoleClient";
  import { providerDisplayName } from "../lib/format";
  import Modal from "../components/Modal.svelte";
  import Alert from "../components/app/Alert.svelte";
  import Field from "../components/app/Field.svelte";
  import Button from "../components/ui/button.svelte";
  import Badge from "../components/ui/badge.svelte";
  import { controlClass, selectClass, monoClass } from "$lib/ui";
  import { cn } from "$lib/utils";

  interface Props {
    ondone: () => void;
  }
  let { ondone }: Props = $props();

  const FALLBACK_KINDS: OAuthProviderMeta[] = [
    {
      kind: "claude",
      display_name: "Claude (Anthropic)",
      flow: "authorization_code_pkce",
      default_base_url: "https://api.anthropic.com",
      callback_port: 54545,
    },
    {
      kind: "codex",
      display_name: "Codex (ChatGPT)",
      flow: "authorization_code_pkce",
      default_base_url: "https://chatgpt.com/backend-api/codex",
      callback_port: 1455,
    },
    {
      kind: "grok",
      display_name: "Grok (xAI device code)",
      flow: "device_code",
      default_base_url: "https://cli-chat-proxy.grok.com/v1",
      callback_port: null,
    },
  ];

  let kinds = $state<OAuthProviderMeta[]>(FALLBACK_KINDS);
  let kind = $state("claude");
  let name = $state("");
  let busy = $state(false);
  let session = $state<OAuthSession | null>(null);
  let completedProviderLabel = $state<string | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  const selectedMeta = $derived(kinds.find((k) => k.kind === kind));
  const isPkce = $derived(!(selectedMeta?.flow ?? "").includes("device"));

  onMount(async () => {
    try {
      const list = await oauthApi.listProviders();
      if (list.length > 0) kinds = list;
    } catch {
      /* fallback list stays */
    }
  });
  onDestroy(() => stopPoll());

  function stopPoll() {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
  }

  async function start() {
    busy = true;
    session = null;
    stopPoll();
    try {
      const s = await oauthApi.start(kind, { name: name || undefined });
      session = s;
      if (s.auth_url && isPkce) {
        window.open(s.auth_url, "_blank", "noopener,noreferrer");
      }
      pollTimer = setInterval(() => void poll(s.session_id), 2000);
    } catch (e: unknown) {
      busy = false;
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function poll(id: string) {
    try {
      const st = await oauthApi.session(id);
      session = st;
      if (st.status === "completed") {
        stopPoll();
        busy = false;
        let label =
          name.trim() ||
          (st.email ? `${kind} (${st.email})` : null) ||
          st.provider_id ||
          "provider";
        if (st.provider_id) {
          try {
            const list = await providersApi.list();
            label = providerDisplayName(list, st.provider_id);
          } catch {
            /* keep fallback label */
          }
        }
        completedProviderLabel = label;
        app.toast(`OAuth completed — ${label}`, "ok");
        ondone();
      } else if (st.status === "error" || st.status === "cancelled") {
        stopPoll();
        busy = false;
        if (st.status === "error") app.toast(st.error || "OAuth error", "error");
      }
    } catch (e: unknown) {
      stopPoll();
      busy = false;
      app.toast(e instanceof Error ? e.message : String(e), "error");
    }
  }

  async function cancel() {
    if (session) {
      try {
        await oauthApi.cancel(session.session_id);
      } catch {
        /* best effort */
      }
    }
    stopPoll();
    busy = false;
    session = null;
  }

  function close() {
    stopPoll();
    ondone();
  }
</script>

<Modal onclose={close} title="OAuth login">
  <p class="mb-3 text-sm text-[var(--text-secondary)]">
    Authorize a Claude / Codex / Grok subscription account. A provider is created
    automatically on completion.
  </p>

  {#if !app.isLoopback && isPkce}
    <Alert variant="warning" class="mb-3">
      Console endpoint is not loopback. PKCE callbacks bind to the daemon machine
      {#if selectedMeta?.callback_port}(port {selectedMeta.callback_port}){/if}
      — the browser must reach that machine. Remote-only? Use Grok device code or
      an API-key provider.
    </Alert>
  {/if}

  <div class="mb-4 grid gap-3 sm:grid-cols-2">
    <Field label="Provider">
      <select class={selectClass} bind:value={kind} disabled={busy}>
        {#each kinds as k (k.kind)}
          <option value={k.kind}>{k.display_name}</option>
        {/each}
      </select>
    </Field>
    <Field label="Display name (optional)">
      <input class={controlClass} bind:value={name} placeholder="My account" disabled={busy} />
    </Field>
  </div>

  {#if session}
    <div class="mb-4 space-y-2 rounded-lg border border-[var(--border)] bg-[var(--surface-muted)]/50 p-3 text-sm">
      <div class="flex items-center gap-2">
        <span class="text-[var(--text-secondary)]">Status</span>
        <Badge
          variant={session.status === "completed"
            ? "success"
            : session.status === "error"
              ? "danger"
              : "secondary"}
        >
          <span class={monoClass}>{session.status}</span>
        </Badge>
      </div>
      {#if session.user_code}
        <p class="text-xs text-[var(--text-secondary)]">Enter this code at the verification page:</p>
        <div class={cn(monoClass, "text-lg font-semibold tracking-wider text-[var(--text)]")}>
          {session.user_code}
        </div>
        {#if session.verification_uri_complete || session.verification_uri}
          <a
            class="break-all text-[var(--accent)] underline-offset-2 hover:underline"
            href={session.verification_uri_complete ?? session.verification_uri}
            target="_blank"
            rel="noreferrer"
          >
            {session.verification_uri_complete ?? session.verification_uri}
          </a>
        {/if}
        {#if session.expires_in}
          <p class="text-xs text-[var(--text-muted)]">Expires in {session.expires_in}s</p>
        {/if}
      {:else if session.auth_url}
        <p class="text-xs text-[var(--text-secondary)]">
          If the browser did not open, complete authorization on the
          <strong>daemon machine</strong>:
        </p>
        <a
          class="break-all text-xs text-[var(--accent)] underline-offset-2 hover:underline"
          href={session.auth_url}
          target="_blank"
          rel="noreferrer"
        >
          {session.auth_url}
        </a>
      {/if}
      {#if session.status === "completed"}
        <p class="text-[var(--success)]">
          Provider created: {completedProviderLabel ?? (name.trim() || session.email || "ok")}
        </p>
      {/if}
      {#if session.error}
        <p class="text-[var(--danger)]">{session.error}</p>
      {/if}
    </div>
  {/if}

  <div class="flex flex-wrap justify-end gap-2">
    {#if !busy && session?.status !== "completed"}
      <Button onclick={start}>Start authorization</Button>
    {/if}
    {#if busy}
      <Button variant="outline" onclick={cancel}>Cancel session</Button>
    {/if}
    <Button variant="outline" onclick={close}>Close</Button>
  </div>
</Modal>
