<script lang="ts">
  import type { HTMLButtonAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";
  import { type VariantProps, cva } from "class-variance-authority";

  const buttonVariants = cva(
    "inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-[10px] text-sm font-medium transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent)] focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 cursor-pointer border border-transparent",
    {
      variants: {
        variant: {
          default:
            "bg-[var(--accent)] text-white shadow-sm hover:bg-[var(--accent-hover)] hover:shadow",
          secondary:
            "bg-[var(--surface-muted)] text-[var(--text)] border-[var(--border)] hover:bg-[var(--border)]",
          outline:
            "border-[var(--border)] bg-[var(--surface)] text-[var(--text)] shadow-sm hover:bg-[var(--surface-muted)] hover:border-[var(--border-strong)]",
          ghost:
            "border-transparent bg-transparent text-[var(--text-secondary)] hover:bg-[var(--surface-muted)] hover:text-[var(--text)]",
          destructive:
            "bg-[var(--danger)] text-white shadow-sm hover:bg-[var(--danger-hover)]",
        },
        size: {
          default: "h-9 px-3.5 py-2",
          sm: "h-8 rounded-lg px-2.5 text-xs",
          lg: "h-10 rounded-[10px] px-4",
          icon: "h-9 w-9",
        },
      },
      defaultVariants: {
        variant: "default",
        size: "default",
      },
    },
  );

  type ButtonVariant = VariantProps<typeof buttonVariants>["variant"];
  type ButtonSize = VariantProps<typeof buttonVariants>["size"];

  interface Props extends HTMLButtonAttributes {
    variant?: ButtonVariant;
    size?: ButtonSize;
    class?: string;
  }

  let {
    variant = "default",
    size = "default",
    class: className = "",
    type = "button",
    children,
    ...rest
  }: Props = $props();
</script>

<button
  {type}
  class={cn(buttonVariants({ variant, size }), className)}
  {...rest}
>
  {@render children?.()}
</button>
