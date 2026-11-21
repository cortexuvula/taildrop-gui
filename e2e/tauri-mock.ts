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
 *   ?lastHost=H                   browser config seed: persisted last-remote H
 *   ?savedUser=host:user          browser config seed: persisted per-host account
 *   ?pins=0                       start with an empty known-hosts pin list
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
  /** Browser (M1) mock surface. */
  browserJobsNow: () => Array<Record<string, unknown>>;
  browserFsNow: (target: string) => unknown;
  browserConfigNow: () => { hosts: Record<string, { user: string | null }>; last: string };
  /** M2a: current known-hosts pin list (Forget mutates it). */
  browserPinsNow: () => Array<{ host: string; flavor: string; fingerprint: string }>;
  emitCopy: (payload: unknown) => void;
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


// ---------------------------------------------------------------------------
// Browser (M1) mock: in-memory filesystem per target + serial copy-job
// simulation with real browser-copy-event emissions. Collisions follow the
// production rules: copying a name that already exists at the destination
// fails with kind "exists" unless replace=true (then it overwrites).
// ---------------------------------------------------------------------------

interface MockNode {
  type: "dir" | "file";
  size?: number;
  mtime?: number;
  perm?: number;
  children?: Record<string, MockNode>;
}

function file(size = 1024): MockNode {
  return { type: "file", size, mtime: 1700000000, perm: 0o644 };
}
function dir(children: Record<string, MockNode>): MockNode {
  return { type: "dir", mtime: 1700000000, perm: 0o755, children };
}

const browserFS: Record<string, MockNode> = {
  local: dir({
    "Documents": dir({ "report.pdf": file(20480) }),
    "notes.txt": file(120),
    "photo.jpg": file(900000),
    ".hiddenrc": file(8),
  }),
  "studio-mac": dir({
    "notes.txt": file(999), // deliberate collision with local/notes.txt
    "incoming": dir({}),
    "music": dir({ "track.flac": file(30000000) }),
  }),
  // M2a: second remote for the remote→remote copy tests (no notes.txt at the
  // root, so studio-mac:/notes.txt → linux-box:/ never collides).
  "linux-box": dir({
    "incoming": dir({}),
    "shared": dir({ "todo.md": file(64) }),
  }),
  winbox: dir({ "stuff": dir({}) }),
};

const browserPeers = [
  {
    host: "studio-mac", name: "studio-mac", dns: "studio-mac.tail.ps.net.",
    ip: "100.79.10.43", os: "macos", online: true, user: "",
    tailscaleSsh: true, browsable: true, isSelf: false,
  },
  {
    host: "linux-box", name: "linux-box", dns: "linux-box.tail.ps.net.",
    ip: "100.79.10.44", os: "linux", online: true, user: "",
    tailscaleSsh: true, browsable: true, isSelf: false,
  },
  {
    host: "winbox", name: "winbox", dns: "winbox.tail.ps.net.",
    ip: "100.44.55.66", os: "windows", online: true, user: "",
    tailscaleSsh: false, browsable: true, isSelf: false,
  },
  {
    host: "pixel", name: "pixel", dns: "pixel.tail.ps.net.",
    ip: "100.77.88.99", os: "android", online: true, user: "",
    tailscaleSsh: true, browsable: false, isSelf: false,
  },
];

// ?manyPeers=N — append N synthetic browsable linux peers so the machine
// menu outgrows its cap (scroll regression test). These peers have no mock
// FS, so picking one just yields the usual listing error pane.
const manyPeers = Number(params.get("manyPeers") ?? 0);
for (let i = 0; i < manyPeers; i++) {
  const name = `peer-${String(i).padStart(2, "0")}`;
  browserPeers.push({
    host: name, name, dns: `${name}.tail.ps.net.`,
    ip: `100.64.0.${i + 1}`, os: "linux", online: true, user: "",
    tailscaleSsh: true, browsable: true, isSelf: false,
  });
}

interface MockJob {
  id: number;
  state: "queued" | "running" | "done" | "failed" | "cancelled";
  label: string;
  pct: number;
  rate: string;
  file: string;
  skipped: number;
  error?: string;
  kind?: string;
  names?: string[];
  request: Record<string, unknown>;
  created: number;
}

// M2a config seeds: ?lastHost= persists the last-remote selection, and
// ?savedUser=host:user persists a per-host account (restart-memory e2e).
function seededConfig(): { hosts: Record<string, { user: string | null }>; last: string } {
  const hosts: Record<string, { user: string | null }> = {};
  const savedUser = params.get("savedUser");
  if (savedUser) {
    const idx = savedUser.indexOf(":");
    if (idx > 0) hosts[savedUser.slice(0, idx)] = { user: savedUser.slice(idx + 1) };
  }
  return { hosts, last: params.get("lastHost") ?? "" };
}

const browserState = {
  jobs: [] as MockJob[],
  nextJobId: 1,
  config: seededConfig(),
  timers: new Map<number, ReturnType<typeof setInterval>>(),
};

// M2a known-hosts pins (mutable — Forget removes rows). ?pins=0 starts empty.
interface MockPin {
  host: string;
  flavor: string;
  fingerprint: string;
}
let browserPins: MockPin[] =
  params.get("pins") === "0"
    ? []
    : [
        {
          host: "studio-mac",
          flavor: "tailscale-ssh",
          fingerprint: "SHA256:WnFigd6vxW1unbyFJHKepROv0zUUHGzEQZiaKA2ez/4",
        },
        { host: "linux-box", flavor: "openssh", fingerprint: "" },
      ];

const browserFlags = {
  failAuth: params.get("browserFailAuth") === "1",
  slowCopy: params.get("slowCopy") === "1",
};

function nodeAt(target: string, path: string): MockNode | null {
  const root = browserFS[target];
  if (!root) return null;
  const clean = (path || "/").replace(/\/+$/, "") || "/";
  if (clean === "/") return root;
  let cur = root;
  for (const part of clean.split("/").filter(Boolean)) {
    if (cur.type !== "dir" || !cur.children || !cur.children[part]) return null;
    cur = cur.children[part];
  }
  return cur;
}

function listingFor(target: string, path: string) {
  const node = nodeAt(target, path);
  if (!node || node.type !== "dir") {
    throw { message: `${path} not found on ${target}`, kind: "notfound" };
  }
  const clean = (path || "/").replace(/\/+$/, "") || "/";
  const entries = Object.entries(node.children ?? {}).map(([name, n]) => ({
    name,
    type: n.type === "dir" ? "dir" : "file",
    link: false,
    size: n.size ?? 0,
    mtime: n.mtime ?? 0,
    perm: n.perm ?? 0o644,
  }));
  entries.sort((a, b) => a.name.localeCompare(b.name));
  return {
    path: clean,
    entries,
    skipped: 0,
    user: target === "local" ? "" : "peeruser",
    home: "/",
  };
}

function startJob(req: Record<string, unknown>): number {
  const id = browserState.nextJobId++;
  const names = (req.names as string[]) ?? [];
  const job: MockJob = {
    id,
    state: "queued",
    label: names.length === 1 ? names[0] : `${names.length} items`,
    pct: 0,
    rate: "",
    file: "",
    skipped: 0,
    request: req,
    created: Date.now(),
  };
  browserState.jobs.push(job);
  emit("browser-copy-event", { jobId: id, event: "start", count: names.length });
  job.state = "running";
  const dstTarget = String(req.dstTarget);
  const dstDir = String(req.dstDir);
  const replace = Boolean(req.replace);
  let pct = 0;
  const tickMs = browserFlags.slowCopy ? 300 : 25;
  const tickPct = browserFlags.slowCopy ? 5 : 34;
  const timer = setInterval(() => {
    pct += tickPct;
    if (pct < 100) {
      job.pct = pct;
      job.rate = "12 MB/s";
      job.file = names[0] ?? "";
      emit("browser-copy-event", {
        jobId: id, event: "progress", pct, rate: job.rate, file: job.file,
      });
      return;
    }
    browserState.timers.delete(id);
    // Actually STOP the interval: without this the settled job keeps ticking,
    // and a no-collision copy flips done→failed on the next pass (the file it
    // just landed now "collides" with itself). The cancel path already clears.
    clearInterval(timer);
    job.pct = 100;
    // Collision rule: any name already at the destination, without consent.
    const collide = names.filter((n) => nodeAt(dstTarget, `${dstDir}/${n}`.replace("//", "/")));
    if (collide.length > 0 && !replace) {
      job.state = "failed";
      job.kind = "exists";
      job.names = collide;
      job.error = `${collide.join(", ")} already exist in ${dstDir}`;
      emit("browser-copy-event", {
        jobId: id, event: "done", ok: false, kind: "exists", names: collide, error: job.error,
      });
      return;
    }
    // Success: apply to the mock FS.
    const dst = nodeAt(dstTarget, dstDir);
    if (dst && dst.type === "dir" && dst.children) {
      const src = nodeAt(String(req.srcTarget), String(req.srcDir));
      for (const n of names) {
        const node = src?.children?.[n];
        dst.children[n] = node ? JSON.parse(JSON.stringify(node)) : file(1);
      }
    }
    job.state = "done";
    emit("browser-copy-event", { jobId: id, event: "done", ok: true, skipped: 0 });
  }, tickMs);
  browserState.timers.set(id, timer);
  return id;
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
    // ---- Browser (M1) commands ----
    case "browser_peers":
      return browserPeers;
    case "browser_config":
      return browserState.config;
    case "browser_probe":
      return {
        system: args?.host === "winbox" ? "Windows 11" : "macOS 15",
        posix: args?.host !== "winbox",
        home: "/",
        flavor: "tailscale-ssh",
        user: "peeruser",
        extensions: { "hardlink@openssh.com": "1" },
      };
    case "browser_ls": {
      const target = String(args?.target);
      if (target !== "local" && browserFlags.failAuth) {
        throw { message: `denied: no tailnet identity for ${target}`, kind: "auth" };
      }
      return listingFor(target, String(args?.path ?? ""));
    }
    case "browser_mkdir": {
      const path = String(args?.path);
      const parent = path.substring(0, path.lastIndexOf("/")) || "/";
      const name = path.substring(path.lastIndexOf("/") + 1);
      const node = nodeAt(String(args?.target), parent);
      if (!node || node.type !== "dir") throw { message: "no such folder", kind: "notfound" };
      if (node.children![name]) throw { message: `${name} already exists`, kind: "exists" };
      node.children![name] = dir({});
      return null;
    }
    case "browser_rename": {
      const oldP = String(args?.old);
      const newP = String(args?.new);
      const t = String(args?.target);
      const oParent = nodeAt(t, oldP.substring(0, oldP.lastIndexOf("/")) || "/");
      const oName = oldP.substring(oldP.lastIndexOf("/") + 1);
      const nName = newP.substring(newP.lastIndexOf("/") + 1);
      if (!oParent?.children?.[oName]) throw { message: "gone", kind: "notfound" };
      const nParent = nodeAt(t, newP.substring(0, newP.lastIndexOf("/")) || "/");
      if (nParent?.children?.[nName]) throw { message: `${nName} already exists`, kind: "exists" };
      nParent!.children![nName] = oParent.children[oName];
      delete oParent.children[oName];
      return null;
    }
    case "browser_delete": {
      const t = String(args?.target);
      const paths = (args?.paths as string[]) ?? [];
      let done = 0;
      for (const pth of paths) {
        const parent = nodeAt(t, pth.substring(0, pth.lastIndexOf("/")) || "/");
        const name = pth.substring(pth.lastIndexOf("/") + 1);
        if (parent?.children?.[name]) {
          delete parent.children[name];
          done++;
        }
      }
      return { trashed: t === "local", done };
    }
    case "browser_open":
      return `/private/tmp/e2e-opened/${String(args?.path).split("/").pop()}`;
    case "browser_copy":
      return startJob((args?.job as Record<string, unknown>) ?? {});
    case "browser_cancel_copy": {
      const id = Number(args?.jobId);
      const job = browserState.jobs.find((j) => j.id === id);
      const timer = browserState.timers.get(id);
      if (timer) clearInterval(timer);
      browserState.timers.delete(id);
      if (job && (job.state === "running" || job.state === "queued")) {
        job.state = "cancelled";
        emit("browser-copy-event", { jobId: id, event: "done", ok: false, cancelled: true });
      }
      return null;
    }
    case "browser_copy_jobs":
      return browserState.jobs.map((j) => ({ ...j }));
    case "browser_set_user": {
      const host = String(args?.host);
      const user = (args?.user as string | null) ?? null;
      if (user === null) delete browserState.config.hosts[host];
      else browserState.config.hosts[host] = { user };
      return null;
    }
    case "browser_set_last":
      browserState.config.last = String(args?.host);
      return null;
    // ---- M2a: known-hosts pin management (M1 gap 4) ----
    case "browser_known_hosts":
      return browserPins.map((p) => ({ ...p }));
    case "browser_forget_host_key": {
      const host = String(args?.host);
      const flavor = String(args?.flavor);
      const before = browserPins.length;
      browserPins = browserPins.filter((p) => !(p.host === host && p.flavor === flavor));
      if (browserPins.length === before) {
        throw { message: `no pinned ${flavor} key for ${host}`, kind: "notfound" };
      }
      return null;
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
  browserJobsNow: () => browserState.jobs.map((j) => ({ ...j })),
  browserFsNow: (target: string) => browserFS[target],
  browserConfigNow: () => browserState.config,
  browserPinsNow: () => browserPins.map((p) => ({ ...p })),
  emitCopy: (payload: unknown) => emit("browser-copy-event", payload),
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
