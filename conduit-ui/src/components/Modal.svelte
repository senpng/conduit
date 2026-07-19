<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  interface Props {
    onclose?: () => void;
    wide?: boolean;
    title?: string;
    children?: Snippet;
  }
  let { onclose, wide = false, title, children }: Props = $props();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="modal-backdrop fixed inset-0 z-50 flex items-start justify-center bg-[rgb(17_24_39/0.35)] pt-[12vh]"
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) onclose?.();
  }}
>
  <div
    class={cn(
      "modal max-h-[80vh] overflow-auto rounded-xl border border-[var(--border)] bg-[var(--surface)] p-5 shadow-[0_8px_30px_rgb(17_24_39/0.08)]",
      wide ? "w-[min(720px,94vw)]" : "w-[min(480px,92vw)]",
    )}
    role="dialog"
    aria-modal="true"
    tabindex="-1"
  >
    {#if title}
      <h3 class="mb-2 text-base font-semibold text-[var(--text)]">{title}</h3>
    {/if}
    {@render children?.()}
  </div>
</div>
