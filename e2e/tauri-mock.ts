/**
 * Browser-side mock of the Tauri v2 IPC surface (`window.__TAURI_INTERNALS__`
 * and `window.__TAURI_EVENT_PLUGIN_INTERNALS__`), installed BEFORE the
 * production App is imported so Playwright can exercise the real components
 * with CSS enabled. The mock routes the same commands the Rust backend
 * exposes; scenarios are selected via URL query parameters:
 *
 *   ?scenario=none|one|ten|long   staging groups present at startup scan
 *   ?incoming=N                   N files in the daemon inbox
 *   ?hangAccept=1                 accept_file never resolves (active transfer)
 *   ?failRecover=1                recover_staging_files rejects
 *   ?failDiscard=1                discard_staging_dir rejects
 *   ?slowRecover=1                recover/discard take ~400ms (busy-state tests)
 *   ?holdRecover=1                recover never settles until __e2e.releaseRecover()
 *   ?holdDiscard=1                discard never settles until __e2e.releaseDiscard()
 */

type InvokeArgs = Record<string, unknown> | undefined;

interface MockStagingFile {
  name: string;
  size: number;
}

interface MockStagingDir {
  path: string;
  files: MockStagingFile[];
}

interface MockReceipt {
  seq: number;
  id: string;
  filename: string;
  savedName: string;
  savedPath: string;
  size: number;
  peerName: string | null;
  direction: "received";
  status: "saved" | "failed" | "salvaged";
  timestamp: number;
}

interface E2eApi {
  calls: Array<{ cmd: string; args: InvokeArgs }>;
  stagingNow: () => MockStagingDir[];
  receiptsNow: () => MockReceipt[];
  emit: (event: string, payload: unknown) => void;
  emitStagingFound: (dirs: MockStagingDir[]) => void;
  /** Resolve a recover held by ?holdRecover=1 (no-op when nothing is held). */
  releaseRecover: () => void;
  /** Resolve a discard held by ?holdDiscard=1 (no-op when nothing is held). */
  releaseDiscard: () => void;
}

declare global {
  interface Window {
    __e2e?: E2eApi;
  }
}

const params = new URLSearchParams(window.location.search);

function tenDuplicateFilenameGroups(): MockStagingDir[] {
  // Ten DISTINCT directories carrying IDENTICAL filenames — nothing may be
  // deduplicated and every row must keep its own stable identity.
  const files = [
    { name: "vacation-photo.jpg", size: 40960 },
    { name: "report-final.pdf", size: 262144 },
    { name: "empty.bin", size: 0 },
  ];
  return Array.from({ length: 10 }, (_, i) => ({
    path: `/tmp/taildrop-accept-${String(i).padStart(4, "0")}`,
    files,
  }));
}

function longPathGroup(): MockStagingDir[] {
  const seg = "very-long-directory-segment";
  const longDir = Array.from({ length: 8 }, () => seg).join("/");
  const longName = `an-extremely-long-received-filename-${"x".repeat(60)}.tar.gz`;
  return [
    {
      path: `/tmp/${longDir}/taildrop-accept-cafebabe`,
      files: [
        { name: longName, size: 4 },
        { name: "tiny.txt", size: 8 },
      ],
    },
  ];
}

function initialStaging(): MockStagingDir[] {
  switch (params.get("scenario")) {
    case "none":
      return [];
    case "ten":
      return tenDuplicateFilenameGroups();
    case "long":
      return longPathGroup();
    case "one":
    default:
      return [
        {
          path: "/tmp/taildrop-accept-deadbeef",
          files: [
            { name: "vacation-photo.jpg", size: 40960 },
            { name: "notes.txt", size: 4 },
          ],
        },
      ];
  }
}

function initialIncoming() {
  const n = Number(params.get("incoming") ?? "0");
  return Array.from({ length: n }, (_, i) => ({
    name: `incoming-${i}.zip`,
    size: 1048576 + i,
  }));
}

const state = {
  staging: initialStaging(),
  receipts: [] as MockReceipt[],
  incoming: initialIncoming(),
  calls: [] as Array<{ cmd: string; args: InvokeArgs }>,
  seq: 0,
};

const flags = {
  failRecover: params.get("failRecover") === "1",
  failDiscard: params.get("failDiscard") === "1",
  hangAccept: params.get("hangAccept") === "1",
  slow: params.get("slowRecover") === "1",
  holdRecover: params.get("holdRecover") === "1",
  holdDiscard: params.get("holdDiscard") === "1",
};

// Resolvers for ops parked by the hold flags; __e2e.release*() settles them.
let releaseRecover: (() => void) | null = null;
let releaseDiscard: (() => void) | null = null;

// ---- Event callback registry (mirrors Tauri's transformCallback protocol) ----
const callbacks = new Map<number, (e: unknown) => void>();
const listeners = new Map<number, string>();
let nextId = 1;

function emit(event: string, payload: unknown) {
  for (const [id, name] of listeners) {
    if (name === event) callbacks.get(id)?.({ event, id, payload });
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const selfPeer = {
  id: "self", public_key: "pk", hostname: "this-machine",
  dns_name: "this-machine.tail.ts.net.", display_name: "This Machine",
  machine_name: "this-machine", os: "macos", ips: ["100.64.0.1"],
  online: true, is_self: true, is_exit_node: false,
};
const otherPeer = {
  id: "peer-1", public_key: "pk2", hostname: "studio-mac",
  dns_name: "studio-mac.tail.ps.net.", display_name: "Studio Mac",
  machine_name: "studio-mac", os: "macos", ips: ["100.79.10.43"],
  online: true, is_self: false, is_exit_node: false,
};

function pushSalvagedReceipts(dir: MockStagingDir) {
  for (const f of dir.files) {
    state.receipts.push({
      seq: ++state.seq,
      id: `salvage-${state.seq}`,
      filename: f.name,
      savedName: f.name,
      savedPath: `/Users/me/Documents/Custom/${f.name}`,
      size: f.size,
      peerName: null,
      direction: "received",
      status: "salvaged",
      timestamp: Date.now(),
    });
  }
}

async function handle(cmd: string, args: InvokeArgs): Promise<unknown> {
  switch (cmd) {
    case "plugin:event|listen": {
      // args.handler is the transformCallback id the API registered — reuse
      // it so emit() can find the callback in the registry.
      const handlerId = Number(args?.handler);
      listeners.set(handlerId, String(args?.event));
      return handlerId;
    }
    case "plugin:event|unlisten": {
      const eventId = Number(args?.eventId);
      listeners.delete(eventId);
      return null;
    }
    case "plugin:app|version":
      return "0.0.0-e2e";
    case "plugin:updater|check":
      return null;
    case "plugin:autostart|is_enabled":
      return false;
    case "plugin:autostart|enable":
      return null;
    case "plugin:autostart|disable":
      return null;
    case "plugin:notification|is_permission_granted":
      return false;
    case "plugin:dialog|open":
      return null;
    case "get_tailscale_status":
      return [selfPeer, otherPeer];
    case "get_debug_logs":
      // DebugPanel harness: empty backend log page (useDebugLogs maps this).
      return [];
    case "get_env_info":
      return "e2e-mock · macOS 15 · WebKit";
    case "get_incoming_files":
      return state.incoming;
    case "get_default_download_dir":
      return "/Users/me/Downloads";
    case "validate_save_dir":
      return null;
    case "set_receive_settings":
      return null;
    case "accept_file":
      if (flags.hangAccept) return new Promise<never>(() => {});
      return "";
    case "get_recent_receipts": {
      const since = Number(args?.sinceSeq ?? 0);
      const page = state.receipts.filter((r) => r.seq > since).slice(0, 50);
      const lastSeq = page.length ? page[page.length - 1].seq : since;
      return {
        receipts: page,
        nextSinceSeq: lastSeq,
        hasMore: state.receipts.some((r) => r.seq > lastSeq),
        reset: false,
      };
    }
    case "staging_recovery_scan":
      return { dirs: state.staging };
    case "recover_staging_files": {
      if (flags.slow) await sleep(400);
      if (flags.holdRecover) {
        await new Promise<void>((resolve) => { releaseRecover = resolve; });
      }
      if (flags.failRecover) throw "recover failed: destination disk full";
      const dir = state.staging.find((d) => d.path === args?.path);
      if (dir) {
        pushSalvagedReceipts(dir);
        state.staging = state.staging.filter((d) => d.path !== args?.path);
      }
      return "ok";
    }
    case "discard_staging_dir": {
      if (flags.slow) await sleep(400);
      if (flags.holdDiscard) {
        await new Promise<void>((resolve) => { releaseDiscard = resolve; });
      }
      if (flags.failDiscard) throw "cannot remove directory";
      state.staging = state.staging.filter((d) => d.path !== args?.path);
      return "ok";
    }
    case "show_in_folder":
    case "send_file":
      return null;
    default:
      return null;
  }
}

window.__TAURI_INTERNALS__ = {
  invoke: (cmd: string, args?: InvokeArgs) => {
    state.calls.push({ cmd, args });
    return handle(cmd, args);
  },
  transformCallback: (cb: (e: unknown) => void) => {
    const id = nextId++;
    callbacks.set(id, cb);
    return id;
  },
  unregisterCallback: (id: number) => {
    callbacks.delete(id);
  },
  metadata: {
    currentWindow: { label: "main" },
    currentWebview: { label: "main" },
  },
  convertFileSrc: (p: string) => p,
};

window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener: (_event: string, eventId: number) => {
    listeners.delete(eventId);
  },
};

window.__e2e = {
  calls: state.calls,
  stagingNow: () => state.staging,
  receiptsNow: () => state.receipts,
  emit,
  emitStagingFound: (dirs) => emit("staging-recovery-found", { dirs }),
  releaseRecover: () => {
    releaseRecover?.();
    releaseRecover = null;
  },
  releaseDiscard: () => {
    releaseDiscard?.();
    releaseDiscard = null;
  },
};

export {};
