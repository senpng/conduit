/**
 * Shared Tailwind class strings for light product chrome.
 * Keeps tables/forms consistent without per-view restyling.
 */

export const controlClass =
  "h-9 w-full appearance-none rounded-[10px] border border-[var(--border)] bg-[var(--surface)] px-3 text-sm text-[var(--text)] shadow-[var(--shadow-sm)] outline-none transition-all placeholder:text-[var(--text-muted)] focus:border-[var(--accent)] focus:ring-[3px] focus:ring-[var(--accent-soft)] disabled:cursor-not-allowed disabled:opacity-50";

export const selectClass = controlClass;

export const textareaClass =
  "min-h-[4.5rem] w-full rounded-[10px] border border-[var(--border)] bg-[var(--surface)] px-3 py-2 font-mono text-sm text-[var(--text)] shadow-[var(--shadow-sm)] outline-none transition-all placeholder:text-[var(--text-muted)] focus:border-[var(--accent)] focus:ring-[3px] focus:ring-[var(--accent-soft)] disabled:cursor-not-allowed disabled:opacity-50";

export const tableWrapClass =
  "overflow-auto rounded-[12px] border border-[var(--border)] bg-[var(--surface)] shadow-[var(--shadow-sm)]";

export const tableClass = "w-full border-collapse text-[13px]";

export const thClass =
  "sticky top-0 z-[1] border-b border-[var(--border)] bg-[var(--surface-muted)] px-3.5 py-2.5 text-left text-[11px] font-semibold tracking-wide text-[var(--text-secondary)] uppercase";

export const tdClass =
  "border-b border-[var(--border)] px-3.5 py-2.5 align-middle text-[var(--text)] last:border-b-0";

export const trHoverClass = "transition-colors hover:bg-[var(--surface-muted)]/80";

export const trClickClass =
  "cursor-pointer transition-colors hover:bg-[var(--surface-muted)]/80";

export const trSelectedClass = "bg-[var(--accent-soft)] hover:bg-[var(--accent-soft)]";

export const monoClass = "font-mono text-[0.92em]";

export const mutedClass = "text-[var(--text-muted)]";

export const dimClass = "text-[var(--text-secondary)]";

export const segmentGroupClass =
  "inline-flex overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--surface)]";

export const segmentBtnClass =
  "border-l border-[var(--border)] px-2.5 py-1.5 text-xs text-[var(--text-secondary)] first:border-l-0 hover:bg-[var(--surface-muted)]";

export const segmentBtnActiveClass =
  "bg-[var(--accent-soft)] font-medium text-[var(--accent)] hover:bg-[var(--accent-soft)]";
