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

  // Focus the overlay on mount; restore focus to the trigger on unmount.
  useEffect(() => {
    if (!enabled) return;
    previouslyFocused.current = document.activeElement as HTMLElement;
    overlayRef.current?.focus();
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
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
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
