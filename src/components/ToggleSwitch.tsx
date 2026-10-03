import { useId } from "react";

interface ToggleSwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  label: string;
  /** Optional smaller variant for inline use (e.g. device list rows). */
  compact?: boolean;
}

/**
 * Animated pill toggle switch with proper ARIA switch role, keyboard
 * activation, and reduced-motion support. Use `compact` for device-list
 * toggles (smaller track, label visually hidden but accessible).
 */
export function ToggleSwitch({
  checked,
  onChange,
  disabled = false,
  label,
  compact = false,
}: ToggleSwitchProps) {
  const id = useId();

  return (
    <label
      className={`toggle-switch ${compact ? "toggle-switch--compact" : ""} ${disabled ? "toggle-switch--disabled" : ""}`}
      htmlFor={id}
    >
      <span
        className={`toggle-switch-label ${compact ? "toggle-switch-label--sr-only" : ""}`}
      >
        {label}
      </span>
      <button
        id={id}
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        className={`toggle-switch-track ${checked ? "toggle-switch-on" : "toggle-switch-off"}`}
        onClick={() => onChange(!checked)}
        type="button"
      >
        <span className="toggle-switch-thumb" />
      </button>
    </label>
  );
}