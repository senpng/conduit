<script lang="ts">
  import Modal from "./Modal.svelte";
  import Button from "./ui/button.svelte";
  import { app } from "../state/app.svelte";
  import type { CreateKeyResponse } from "../lib/consoleClient";
  import { Copy, Check } from "@lucide/svelte";

  interface Props {
    keyData: CreateKeyResponse;
    onclose: () => void;
  }
  let { keyData, onclose }: Props = $props();

  let copied = $state(false);

  async function copyKey() {
    try {
      await navigator.clipboard.writeText(keyData.key);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      app.toast("Clipboard write failed — copy manually", "warn");
    }
  }

  function close() {
    onclose();
  }
</script>

<Modal onclose={close} title={`Key created — ${keyData.name}`}>
  <p class="mb-3 text-sm text-[var(--text-secondary)]">
    Copy this key now. <strong class="text-[var(--text)]">It will not be shown again</strong> after
    this dialog closes.
  </p>
  <div
    class="mb-4 flex items-center gap-2 rounded-lg border border-[var(--border)] bg-[var(--surface-muted)] p-3"
  >
    <code class="min-w-0 flex-1 break-all font-mono text-xs text-[var(--text)]">{keyData.key}</code>
    <Button variant="outline" size="sm" onclick={copyKey}>
      {#if copied}
        <Check class="h-3.5 w-3.5" />
        Copied
      {:else}
        <Copy class="h-3.5 w-3.5" />
        Copy
      {/if}
    </Button>
  </div>
  <div class="flex justify-end">
    <Button onclick={close}>Done</Button>
  </div>
</Modal>
