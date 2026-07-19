<script lang="ts">
  import { cn } from "$lib/utils";

  interface Props {
    class?: string;
    label?: string;
  }

  let { class: className = "", label = "Loading…" }: Props = $props();
</script>

<div class={cn("flex items-center justify-center gap-2 py-10 text-sm text-[var(--text-muted)]", className)} role="status">
  <span
    class="h-4 w-4 animate-spin rounded-full border-2 border-[var(--border-strong)] border-t-[var(--accent)]"
    aria-hidden="true"
  ></span>
  <span>{label}</span>
</div>
