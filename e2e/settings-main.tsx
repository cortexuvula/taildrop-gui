// Browser harness for the Settings redesign: mounts the PRODUCTION Settings
// component (or DebugPanel via ?panel=debug) with the production useModal
// hook, production ToastProvider, and the real App.css. Only external
// boundaries are mocked: Tauri IPC via e2e/tauri-mock.ts (imported first so
// window.__TAURI_INTERNALS__ exists before any plugin import runs), and the
// updater via a plain stub object — `updater` is already a prop, so no module
// mocking is needed. Settings state is driven from tests through
// window.__settingsHarness.
//
// The mock import must come first: plugin modules capture the internals at
// import time.
import "./tauri-mock";
import { createRoot } from "react-dom/client";
import { useEffect, useRef, useState } from "react";
import { Settings } from "../src/components/Settings";
import { DebugPanel } from "../src/components/DebugPanel";
import { ToastProvider } from "../src/components/ToastProvider";
import type { UseUpdaterApi, UpdateStatus } from "../src/hooks/useUpdater";
import type { AppSettings, Peer } from "../src/types";
import "../src/App.css";

/** Deterministic synthetic tailnet: one self node plus `count` devices with
 * searchable anchors ("Studio Mac", "Pixel 8"), long display names, mixed
 * OSes/online/exit-node flags, and predictable ids (`dev-N`). */
function makePeers(count: number): Peer[] {
  const self: Peer = {
    id: "self",
    public_key: "pk-self",
    hostname: "this-machine",
    dns_name: "this-machine.tail.ts.net.",
    display_name: "This Machine",
    machine_name: "this-machine",
    os: "macos",
    ips: ["100.64.0.1"],
    online: true,
    is_self: true,
    is_exit_node: false,
  };
  const oses = ["macos", "windows", "linux", "android", "ios"];
  const anchors = ["Studio Mac", "Pixel 8"];
  const devices: Peer[] = Array.from({ length: count }, (_, i) => ({
    id: `dev-${i}`,
    public_key: `pk-${i}`,
    hostname: `host-${i}`,
    dns_name: `host-${i}.tail.ts.net.`,
    display_name:
      (anchors[i] ?? `Device-${String(i).padStart(2, "0")}`) +
      " — a device whose display name is deliberately long enough to need truncation",
    machine_name: `host-${i}`,
    os: oses[i % oses.length],
    ips: [`100.64.${Math.floor(i / 250)}.${(i % 250) + 2}`],
    online: i % 3 !== 2,
    is_self: false,
    is_exit_node: i % 7 === 6,
  }));
  return [self, ...devices];
}

const INITIAL_SETTINGS: AppSettings = {
  hiddenNodes: [],
  saveDirectory: "/Users/me/Downloads",
  autoAccept: false,
  showOfflineNodes: false,
  showExitNodes: false,
  notifications: false,
};

interface HarnessState {
  open: boolean;
  settings: AppSettings;
  closeCount: number;
  /** Every partial passed to onUpdate, in order. */
  updates: Partial<AppSettings>[];
  updaterCheckCalls: number;
}

interface SettingsHarness {
  state(): HarnessState;
  /** Reopen the dialog the way a user does: focus the trigger, then mount. */
  open(): void;
  setSettings(partial: Partial<AppSettings>): void;
  setPeers(count: number): void;
  setSaveDirError(message: string | null): void;
  setUpdaterStatus(status: UpdateStatus): void;
}

declare global {
  interface Window {
    __settingsHarness?: SettingsHarness;
  }
}

function SettingsHarnessApp() {
  // Start closed and open from a focused trigger in a mount effect — the
  // same flow as production, where the dialog only ever opens from a click
  // on the sidebar button. This gives useModal a real element to restore
  // focus to on close.
  const [open, setOpen] = useState(false);
  const [settings, setSettings] = useState<AppSettings>(INITIAL_SETTINGS);
  const [peers, setPeers] = useState<Peer[]>(() => makePeers(30));
  const [saveDirError, setSaveDirError] = useState<string | null>(null);
  const [updaterStatus, setUpdaterStatus] = useState<UpdateStatus>("idle");

  const closeCount = useRef(0);
  const updates = useRef<Partial<AppSettings>[]>([]);
  const updaterCheckCalls = useRef(0);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const updater: UseUpdaterApi = {
    status: updaterStatus,
    version: updaterStatus === "available" ? "9.9.9" : undefined,
    check: async () => {
      updaterCheckCalls.current += 1;
      return updaterStatus === "error" ? "error" : "idle";
    },
    download: async () => {},
    install: async () => {},
    dismiss: () => {},
  };

  const onUpdate = (update: Partial<AppSettings>) => {
    updates.current.push(update);
    setSettings((prev) => ({ ...prev, ...update }));
  };

  const onClose = () => {
    closeCount.current += 1;
    setOpen(false);
  };

  useEffect(() => {
    triggerRef.current?.focus();
    setOpen(true);
  }, []);

  const harness: SettingsHarness = {
    state: () => ({
      open,
      settings,
      closeCount: closeCount.current,
      updates: [...updates.current],
      updaterCheckCalls: updaterCheckCalls.current,
    }),
    open: () => {
      // Mirrors the real trigger flow so useModal's focus-restore has a
      // trigger to return to.
      triggerRef.current?.focus();
      setOpen(true);
    },
    setSettings: (partial) => setSettings((prev) => ({ ...prev, ...partial })),
    setPeers: (count) => setPeers(makePeers(count)),
    setSaveDirError,
    setUpdaterStatus,
  };
  window.__settingsHarness = harness;

  return (
    <div className="app">
      <div className="main">
        <button
          ref={triggerRef}
          id="trigger-settings"
          onClick={() => {
            triggerRef.current?.focus();
            setOpen(true);
          }}
        >
          Open Settings
        </button>
      </div>
      {open && (
        <Settings
          settings={settings}
          allPeers={peers}
          onUpdate={onUpdate}
          onClose={onClose}
          updater={updater}
          saveDirError={saveDirError}
        />
      )}
    </div>
  );
}

function DebugHarnessApp() {
  const peers = makePeers(30);
  return (
    <div className="app">
      <div className="main" />
      <DebugPanel peers={peers} onClose={() => {}} />
    </div>
  );
}

const params = new URLSearchParams(window.location.search);
const root = createRoot(document.getElementById("root") as HTMLElement);
root.render(
  <ToastProvider>
    {params.get("panel") === "debug" ? <DebugHarnessApp /> : <SettingsHarnessApp />}
  </ToastProvider>,
);
