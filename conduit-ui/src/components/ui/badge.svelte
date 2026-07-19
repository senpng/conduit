<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";
  import { type VariantProps, cva } from "class-variance-authority";

  const badgeVariants = cva(
    "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium transition-colors",
    {
      variants: {
        variant: {
          default:
            "border-transparent bg-[var(--accent-soft)] text-[var(--accent)]",
          secondary:
            "border-[var(--border)] bg-[var(--surface-muted)] text-[var(--text-secondary)]",
          success:
            "border-transparent bg-[var(--success-soft)] text-[var(--success)]",
          warning:
            "border-transparent bg-[var(--warning-soft)] text-[var(--warning)]",
          danger:
            "border-transparent bg-[var(--danger-soft)] text-[var(--danger)]",
          outline: "border-[var(--border)] text-[var(--text-secondary)]",
        },
      },
      defaultVariants: {
        variant: "default",
      },
    },
  );

  type BadgeVariant = VariantProps<typeof badgeVariants>["variant"];

  interface Props extends HTMLAttributes<HTMLDivElement> {
    variant?: BadgeVariant;
    class?: string;
  }

  let {
    variant = "default",
    class: className = "",
    children,
    ...rest
  }: Props = $props();
</script>

<div class={cn(badgeVariants({ variant }), className)} {...rest}>
  {@render children?.()}
</div>
