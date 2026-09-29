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
