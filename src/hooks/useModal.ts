import { useEffect, useRef, type HTMLAttributes } from "react";

/**
 * All potentially focusable elements inside a modal. Includes <summary>
 * (focusable via native details disclosure) plus the usual controls.
 */
const FOCUSABLE_SELECTOR =
  'button, [href], input, select, textarea, summary, [tabindex]';

/**
 * Filter to the elements that can actually take focus: not disabled
 * (a disabled button must never receive focus or join the Tab cycle),
 * not hidden, and without tabindex="-1".
 */
function getFocusable(root: HTMLElement): HTMLElement[] {
  const nodes = root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
  return Array.from(nodes).filter(
    (el) =>
      !el.hasAttribute("disabled") &&
      el.tabIndex !== -1 &&
      !el.hasAttribute("hidden"),
  );
}

/**
 * Shared modal hook: handles Escape-to-close, initial focus, focus restore,
 * background inertness, and a focus trap (Tab cycles within the overlay).
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

  // Background inertness: while the modal is open, everything outside the
  // overlay is made inert. Walks up from the overlay to <body>, inerting each
  // sibling subtree along the way — this covers overlays rendered deep inside
  // the React tree (e.g. inside .main), not just direct body children.
  // Declared before the focus effect so its cleanup (un-inert) runs before
  // focus is restored to the trigger on close.
  useEffect(() => {
    if (!enabled) return;
    const overlay = overlayRef.current;
    if (!overlay) return;
    const inerted: Element[] = [];
    let scope: HTMLElement | null = overlay;
    while (scope && scope !== document.body) {
      const parent: HTMLElement | null = scope.parentElement;
      if (!parent) break;
      for (const child of Array.from(parent.children)) {
        if (child !== scope && !child.contains(overlay)) {
          child.setAttribute("inert", "");
          inerted.push(child);
        }
      }
      scope = parent;
    }
    return () => {
      for (const el of inerted) el.removeAttribute("inert");
    };
  }, [enabled]);

  // Focus the first focusable element on mount; restore focus to the trigger on unmount.
  useEffect(() => {
    if (!enabled) return;
    previouslyFocused.current = document.activeElement as HTMLElement;
    const overlay = overlayRef.current;
    if (overlay) {
      const focusable = getFocusable(overlay);
      (focusable[0] || overlay).focus();
    }
    return () => {
      previouslyFocused.current?.focus();
    };
  }, [enabled]);

  // Focus trap: cycle Tab within the overlay, skipping disabled controls.
  useEffect(() => {
    if (!enabled) return;
    const overlay = overlayRef.current;
    if (!overlay) return;

    const handleTab = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const focusable = getFocusable(overlay);
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
