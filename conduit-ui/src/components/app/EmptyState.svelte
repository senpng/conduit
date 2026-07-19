<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  interface Props {
    title?: string;
    class?: string;
    children?: Snippet;
  }

  let { title, class: className = "", children }: Props = $props();
</script>

<div
  class={cn(
    "rounded-lg border border-dashed border-[var(--border)] bg-[var(--surface-muted)]/50 px-4 py-10 text-center text-sm text-[var(--text-muted)]",
    className,
  )}
>
  {#if title}
    <p class="font-medium text-[var(--text-secondary)]">{title}</p>
  {/if}
  {#if children}
    <div class="mt-1">{@render children()}</div>
  {/if}
</div>
