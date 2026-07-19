<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  interface Props {
    title: string;
    description?: string;
    class?: string;
    actions?: Snippet;
  }

  let { title, description, class: className = "", actions }: Props = $props();
</script>

<div class={cn("mb-4 flex flex-wrap items-start justify-between gap-3", className)}>
  <div class="min-w-0">
    <h2 class="text-sm font-semibold text-[var(--text)]">{title}</h2>
    {#if description}
      <p class="mt-0.5 text-xs text-[var(--text-secondary)]">{description}</p>
    {/if}
  </div>
  {#if actions}
    <div class="flex flex-wrap items-center gap-2">
      {@render actions()}
    </div>
  {/if}
</div>
