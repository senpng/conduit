<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    onclose?: () => void;
    wide?: boolean;
    children?: Snippet;
  }
  let { onclose, wide = false, children }: Props = $props();

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose?.();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="modal-backdrop"
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) onclose?.();
  }}
>
  <div class="modal" class:wide role="dialog" aria-modal="true" tabindex="-1">
    {@render children?.()}
  </div>
</div>
