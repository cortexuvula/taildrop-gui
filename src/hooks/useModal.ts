import { useEffect, useRef, type HTMLAttributes } from "react";

/**
 * Shared modal hook: handles Escape-to-close, initial focus, focus restore,
 * and a basic focus trap (Tab cycles within the overlay).
 *
 * Usage:
 *   const { overlayRef, overlayProps } = useModal(onClose);
 *   return <div ref={overlayRef} {...overlayProps} onClick={onClose}>...</div>;
 */
export function useModal(onClose: () => void, enabled: boolean = true) {
  const overlayRef = useRef<HTMLDivElement>(null);
  const previouslyFocused = useRef<HTMLElement | null>(null);

  // Escape to close.
  useEffect(() => {
    if (!enabled) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [onClose, enabled]);

  // Focus the first focusable element on mount; restore focus to the trigger on unmount.
  useEffect(() => {
    if (!enabled) return;
    previouslyFocused.current = document.activeElement as HTMLElement;
    const overlay = overlayRef.current;
    if (overlay) {
      const first = overlay.querySelector<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      );
      (first || overlay).focus();
    }
    return () => {
      previouslyFocused.current?.focus();
    };
  }, [enabled]);

  // Focus trap: cycle Tab within the overlay.
  useEffect(() => {
    if (!enabled) return;
    const overlay = overlayRef.current;
    if (!overlay) return;

    const handleTab = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const focusable = overlay.querySelectorAll<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      );
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      // Handle backward navigation from the overlay container itself or first child
      if (e.shiftKey && (active === overlay || active === first)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };

    overlay.addEventListener("keydown", handleTab);
    return () => overlay.removeEventListener("keydown", handleTab);
  }, [enabled]);

  const overlayProps: HTMLAttributes<HTMLDivElement> = {
    role: "dialog",
    "aria-modal": true,
    tabIndex: -1,
  };

  return { overlayRef, overlayProps };
}

/**
 * Extended variant accepting an optional labelledBy ID for aria-labelledby.
 * Callers with a heading element pass its ID; others omit it (no broken ref).
 */
export function useModalWithLabel(
  onClose: () => void,
  enabled: boolean = true,
  labelledBy?: string,
) {
  const base = useModal(onClose, enabled);
  if (labelledBy) {
    base.overlayProps["aria-labelledby"] = labelledBy;
  }
  return base;
}
