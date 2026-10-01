// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/react";
import { useModalWithLabel } from "../useModal";

/**
 * These tests exercise the actual useModal hook (not a static HTML fixture)
 * so the focus-trap regression check fails if the production hook regresses.
 */

afterEach(cleanup);

function TestDialog({
  onClose,
  labelledBy,
}: {
  onClose: () => void;
  labelledBy?: string;
}) {
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, labelledBy);
  return (
    <div ref={overlayRef} {...overlayProps} data-testid="overlay">
      <button data-testid="first">First</button>
      <input data-testid="middle" type="text" />
      <button data-testid="last">Last</button>
    </div>
  );
}

function TestDialogWithDisabled({
  onClose,
}: {
  onClose: () => void;
}) {
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true);
  return (
    <div ref={overlayRef} {...overlayProps} data-testid="overlay">
      <button data-testid="disabled-first" disabled>Disabled First</button>
      <button data-testid="enabled">Enabled</button>
      <button data-testid="disabled-last" disabled>Disabled Last</button>
    </div>
  );
}

function TestDialogWithSummary({
  onClose,
}: {
  onClose: () => void;
}) {
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true);
  return (
    <div ref={overlayRef} {...overlayProps} data-testid="overlay">
      <details>
        <summary data-testid="summary">Group</summary>
        <button data-testid="btn">Button</button>
      </details>
    </div>
  );
}

describe("useModal focus trap", () => {
  it("Tab from last focusable wraps to first", () => {
    render(<TestDialog onClose={() => {}} />);
    const last = document.querySelector('[data-testid="last"]') as HTMLElement;
    const first = document.querySelector('[data-testid="first"]') as HTMLElement;

    last.focus();
    expect(document.activeElement).toBe(last);

    fireEvent.keyDown(last, { key: "Tab" });
    expect(document.activeElement).toBe(first);
  });

  it("Shift+Tab from first focusable wraps to last", () => {
    render(<TestDialog onClose={() => {}} />);
    const first = document.querySelector('[data-testid="first"]') as HTMLElement;
    const last = document.querySelector('[data-testid="last"]') as HTMLElement;

    first.focus();
    expect(document.activeElement).toBe(first);

    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("Escape fires the onClose callback", () => {
    const onClose = vi.fn();
    render(<TestDialog onClose={onClose} />);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("focuses first focusable element on mount", () => {
    render(<TestDialog onClose={() => {}} />);
    const first = document.querySelector('[data-testid="first"]') as HTMLElement;
    expect(document.activeElement).toBe(first);
  });
});

describe("useModal disabled-control exclusion", () => {
  it("skips disabled controls when choosing initial focus", () => {
    render(<TestDialogWithDisabled onClose={() => {}} />);
    const enabled = document.querySelector('[data-testid="enabled"]') as HTMLElement;
    expect(document.activeElement).toBe(enabled);
  });

  it("does not cycle Tab through disabled controls", () => {
    render(<TestDialogWithDisabled onClose={() => {}} />);
    const enabled = document.querySelector('[data-testid="enabled"]') as HTMLElement;
    const disabledLast = document.querySelector('[data-testid="disabled-last"]') as HTMLElement;

    // Only one focusable control: Tab and Shift+Tab both keep focus on it.
    fireEvent.keyDown(enabled, { key: "Tab" });
    expect(document.activeElement).toBe(enabled);
    fireEvent.keyDown(enabled, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(enabled);
    expect(document.activeElement).not.toBe(disabledLast);
  });
});

describe("useModal summary focusability", () => {
  it("treats details summary as focusable (initial focus and trap)", () => {
    render(<TestDialogWithSummary onClose={() => {}} />);
    const summary = document.querySelector('[data-testid="summary"]') as HTMLElement;
    const btn = document.querySelector('[data-testid="btn"]') as HTMLElement;

    // Summary is the first focusable element in DOM order.
    expect(document.activeElement).toBe(summary);

    // Shift+Tab from the first focusable (the summary) wraps to the last
    // (the button) — the details element itself is never focused.
    fireEvent.keyDown(summary, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(btn);

    // Tab from the last focusable wraps back to the summary.
    fireEvent.keyDown(btn, { key: "Tab" });
    expect(document.activeElement).toBe(summary);
  });
});

describe("useModal background inertness", () => {
  it("inerts sibling subtrees while open and restores them on unmount", () => {
    function App({ open }: { open: boolean }) {
      return (
        <div data-testid="app-root">
          <aside data-testid="sidebar">
            <button data-testid="sidebar-btn">Sidebar</button>
          </aside>
          <main data-testid="main-area">
            {open && <TestDialog onClose={() => {}} />}
          </main>
        </div>
      );
    }

    const { rerender } = render(<App open />);
    const sidebar = document.querySelector('[data-testid="sidebar"]') as HTMLElement;
    expect(sidebar.hasAttribute("inert")).toBe(true);

    rerender(<App open={false} />);
    expect(sidebar.hasAttribute("inert")).toBe(false);
  });
});

describe("useModalWithLabel", () => {
  it("sets aria-labelledby when an ID is provided", () => {
    const { getByTestId } = render(
      <TestDialog onClose={() => {}} labelledBy="dialog-heading" />,
    );
    const overlay = getByTestId("overlay");
    expect(overlay.getAttribute("aria-labelledby")).toBe("dialog-heading");
    expect(overlay.getAttribute("aria-modal")).toBe("true");
    expect(overlay.getAttribute("role")).toBe("dialog");
  });

  it("omits aria-labelledby when no ID is provided", () => {
    const { getByTestId } = render(<TestDialog onClose={() => {}} />);
    const overlay = getByTestId("overlay");
    expect(overlay.hasAttribute("aria-labelledby")).toBe(false);
    // Other required attributes still present
    expect(overlay.getAttribute("aria-modal")).toBe("true");
    expect(overlay.getAttribute("role")).toBe("dialog");
  });
});
