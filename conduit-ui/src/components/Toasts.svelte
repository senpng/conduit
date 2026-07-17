<script lang="ts">
  import { app } from "../state/app.svelte";
</script>

<div class="toast-stack">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.kind}" role="status">
      <span>{t.text}</span>
      <button aria-label="dismiss" onclick={() => app.dismissToast(t.id)}>✕</button>
    </div>
  {/each}
</div>
