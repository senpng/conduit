<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  interface Props {
    label: string;
    hint?: string;
    class?: string;
    children?: Snippet;
  }

  let { label, hint, class: className = "", children }: Props = $props();
</script>

<label class={cn("flex flex-col gap-1.5 text-xs font-medium text-[var(--text-secondary)]", className)}>
  <span>{label}</span>
  {@render children?.()}
  {#if hint}
    <span class="font-normal text-[11px] text-[var(--text-muted)]">{hint}</span>
  {/if}
</label>
