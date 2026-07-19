<script lang="ts">
  import { app } from "../state/app.svelte";
  import Modal from "./Modal.svelte";
  import Button from "./ui/button.svelte";

  let cancelBtn: HTMLButtonElement | undefined = $state();

  $effect(() => {
    if (app.confirm) {
      queueMicrotask(() => cancelBtn?.focus());
    }
  });
</script>

{#if app.confirm}
  <Modal onclose={() => app.settleConfirm(false)} title={app.confirm.title}>
    <p class="mb-4 text-sm text-[var(--text-secondary)]">{app.confirm.body}</p>
    <div class="flex justify-end gap-2">
      <Button
        variant={app.confirm.danger ? "destructive" : "default"}
        onclick={() => app.settleConfirm(true)}
      >
        {app.confirm.confirmLabel}
      </Button>
      <button
        type="button"
        class="inline-flex h-9 cursor-pointer items-center justify-center gap-1.5 rounded-lg border border-[var(--border)] bg-[var(--surface)] px-3.5 text-sm font-medium text-[var(--text)] hover:bg-[var(--surface-muted)]"
        bind:this={cancelBtn}
        onclick={() => app.settleConfirm(false)}
      >
        Cancel
      </button>
    </div>
  </Modal>
{/if}
