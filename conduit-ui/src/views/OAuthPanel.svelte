<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { app } from "../state/app.svelte";
  import { oauth as oauthApi, providers as providersApi } from "../lib/consoleClient";
  import type { OAuthProviderMeta, OAuthSession } from "../lib/consoleClient";
  import { providerDisplayName } from "../lib/format";
  import Modal from "../components/Modal.svelte";

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
  /** Resolved after OAuth completes for display (prefer name over raw id). */
  let completedProviderLabel = $state<string | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  const selectedMeta = $derived(kinds.find((k) => k.kind === kind));
  // Daemon flow values: "authorization_code_pkce" | "device_code".
  const isPkce = $derived(!(selectedMeta?.flow ?? "").includes("device"));

  onMount(async () => {
    try {
      const list = await oauthApi.listProviders();
      if (list.length > 0) kinds = list;
    } catch {
      // fallback list stays
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

<Modal onclose={close}>
  <h3>OAuth login</h3>
  <p class="modal-hint">
    Authorize a Claude / Codex / Grok subscription account. A provider is created
    automatically on completion.
  </p>

  {#if !app.isLoopback && isPkce}
    <div class="warn-bar">
      ⚠ Console endpoint is not loopback. PKCE callbacks bind to the daemon machine
      {#if selectedMeta?.callback_port}(port {selectedMeta.callback_port}){/if}
      — the browser must reach that machine. Remote-only? Use Grok device code or
      an API-key provider.
    </div>
  {/if}

  <div class="form-grid">
    <label>
      Provider
      <select bind:value={kind} disabled={busy}>
        {#each kinds as k (k.kind)}
          <option value={k.kind}>{k.display_name}</option>
        {/each}
      </select>
    </label>
    <label>
      Display name (optional)
      <input bind:value={name} placeholder="My account" disabled={busy} />
    </label>
  </div>

  {#if session}
    <div class="oauth-status">
      <div>
        Status: <strong class="mono">{session.status}</strong>
      </div>
      {#if session.user_code}
        <div class="dim small">Enter this code at the verification page:</div>
        <div class="device-code">{session.user_code}</div>
        {#if session.verification_uri_complete || session.verification_uri}
          <a
            href={session.verification_uri_complete ?? session.verification_uri}
            target="_blank"
            rel="noreferrer"
          >
            {session.verification_uri_complete ?? session.verification_uri}
          </a>
        {/if}
        {#if session.expires_in}
          <div class="dim small">Expires in {session.expires_in}s</div>
        {/if}
      {:else if session.auth_url}
        <div class="dim small">
          If the browser did not open, complete authorization on the
          <strong>daemon machine</strong> via:
        </div>
        <a href={session.auth_url} target="_blank" rel="noreferrer">{session.auth_url}</a>
      {/if}
      {#if session.status === "completed"}
        <div class="ok">
          ✓ Provider created: {completedProviderLabel ??
            (name.trim() || session.email || "ok")}
          {#if session.email && !(completedProviderLabel ?? "").includes(session.email)}
            <span class="dim">({session.email})</span>
          {/if}
        </div>
      {/if}
      {#if session.error}
        <div class="err">{session.error}</div>
      {/if}
    </div>
  {/if}

  <div class="form-actions">
    {#if !busy && session?.status !== "completed"}
      <button class="btn-primary" onclick={start}>Start authorization</button>
    {/if}
    {#if busy}
      <button class="btn-ghost" onclick={cancel}>Cancel session</button>
    {/if}
    <button class="btn-ghost" onclick={close}>Close</button>
  </div>
</Modal>
