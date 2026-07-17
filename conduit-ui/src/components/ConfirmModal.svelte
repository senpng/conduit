<script lang="ts">
  import { app } from "../state/app.svelte";
  import Modal from "./Modal.svelte";

  let cancelBtn: HTMLButtonElement | undefined = $state();

  // Default focus = Cancel (design doc R-7).
  $effect(() => {
    if (app.confirm) {
      queueMicrotask(() => cancelBtn?.focus());
    }
  });

  function onKeydown(e: KeyboardEvent) {
    if (!app.confirm) return;
    if (e.key === "y" || e.key === "Y") {
      e.preventDefault();
      e.stopPropagation();
      app.settleConfirm(true);
    } else if (e.key === "n" || e.key === "N") {
      e.preventDefault();
      e.stopPropagation();
      app.settleConfirm(false);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if app.confirm}
  <Modal onclose={() => app.settleConfirm(false)}>
    <h3>{app.confirm.title}</h3>
    <p class="modal-hint">{app.confirm.body}</p>
    <div class="form-actions">
      <button
        class={app.confirm.danger ? "btn-danger" : "btn-primary"}
        onclick={() => app.settleConfirm(true)}
      >
        {app.confirm.confirmLabel} <span class="kbd">y</span>
      </button>
      <button class="btn-ghost" bind:this={cancelBtn} onclick={() => app.settleConfirm(false)}>
        Cancel <span class="kbd">n</span>
      </button>
    </div>
  </Modal>
{/if}
