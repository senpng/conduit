<script lang="ts">
  import Modal from "./Modal.svelte";
  import { app } from "../state/app.svelte";
  import type { CreateKeyResponse } from "../lib/adminClient";

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
    // Plaintext is dropped with the component state.
    onclose();
  }
</script>

<Modal onclose={close}>
  <h3>Key created — {keyData.name}</h3>
  <p class="modal-hint">
    Copy this key now. <strong>It will not be shown again</strong> after this dialog
    closes.
  </p>
  <div class="key-display">
    <code>{keyData.key}</code>
    <button class="btn-ghost btn-sm" onclick={copyKey}>
      {copied ? "✓ Copied" : "Copy"}
    </button>
  </div>
  <div class="form-actions">
    <button class="btn-primary" onclick={close}>Done</button>
  </div>
</Modal>
