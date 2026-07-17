# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
npm install                  # Install frontend dependencies
npm run tauri dev            # Development mode with hot reload (frontend + backend)
npm run tauri build          # Production build (outputs platform installers)
npm run build                # Frontend-only build (tsc + vite)
npx tsc --noEmit             # TypeScript type-check without emitting
```

Rust-side checks (from `src-tauri/`):
```bash
cargo check                  # Type-check Rust code
cargo clippy                 # Lint Rust code
```

There are no test suites configured. CI runs debug builds on all three platforms as validation.

Rust unit tests live inside the Linux `platform` module in `tailscale.rs` (url_encode, prettify_name, unique_save_path). They only compile/run on Linux (`#[cfg(all(unix, not(target_os = "macos")))]`).

## Architecture

Tauri 2.0 desktop app: **React 19 + TypeScript** frontend communicating with a **Rust** backend via Tauri's IPC bridge (`invoke()`).

### Frontend → Backend Flow

Frontend calls `invoke("command_name", { camelCaseArgs })` which Tauri auto-converts to `snake_case` Rust parameters. All IPC commands are defined in `src-tauri/src/lib.rs` and delegate to `src-tauri/src/tailscale.rs` (Tailscale operations) or `src-tauri/src/debug_log.rs` (logging). The IPC commands: `get_tailscale_status`, `send_file`, `get_incoming_files` (takes `save_dir`), `accept_file`, `get_default_download_dir`, `get_debug_logs`, `get_env_info`.

### Platform-Specific Rust Modules

`tailscale.rs` contains three `mod platform` blocks behind `#[cfg]` gates:

- **Linux** (`cfg(all(unix, not(target_os = "macos")))`): Communicates via Unix socket at `/var/run/tailscale/tailscaled.sock` using `hyperlocal` + `hyper`. Uses the Tailscale localapi directly (HTTP GET/PUT/DELETE).
- **macOS** (`cfg(target_os = "macos")`): Tries the Unix socket first for all operations; falls back to the `tailscale` CLI when the socket is unavailable (App Store installs use a TCP loopback port). When the socket is inaccessible for incoming-file detection, `try_cli_receive_files` auto-downloads pending files to the save directory via `tailscale file get --wait=false --conflict=overwrite`.
- **Windows** (`cfg(windows)`): Tries the Tailscale named pipe (`\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled`) for incoming-file detection; falls back to `tailscale.exe file get` CLI auto-receive when the pipe is inaccessible (non-admin). Sends via CLI with `CREATE_NO_WINDOW`.

All three expose the same public async functions: `fetch_status_json`, `send_file`, `get_incoming_files(save_dir)`, `accept_file`. macOS/Windows wrap blocking `Command::output()` in `tokio::task::spawn_blocking`. `get_incoming_files` takes `save_dir` because on macOS/Windows the CLI fallback auto-downloads files to that directory (there is no pure "list pending files" CLI command).

**Key difference**: Linux localapi needs the peer's stable node ID (`peer_id`), while macOS/Windows CLI uses hostname (`peer_name`). Both are passed through the IPC; each platform uses what it needs.

### Frontend State Management

`src/hooks/useTailscale.ts` is the single state hook — manages peers, transfers, settings, and polling. Peers refresh every 10s, incoming files every 8s (adaptive: 2s when transfers are active). Settings persist to `localStorage`.

Peer filtering logic: only shows peers on the same tailnet (derived from self node's DNS name), with toggles for offline nodes and Mullvad/exit nodes.

### File Transfer Flow

Files are sent by **path** (not content). DropZone uses Tauri's native `onDragDropEvent` for drag-and-drop (returns file paths) and `@tauri-apps/plugin-dialog` `open()` for browse. Rust reads the file from disk — on macOS/Windows it passes the path directly to `tailscale file cp`, on Linux it reads bytes and PUTs to the localapi.

### Debug Logging

`src-tauri/src/debug_log.rs` implements a custom `log::Log` sink (`InMemorySink`) that captures all `log::*` output (debug+) into a bounded 500-line buffer. It wraps `env_logger` so stderr output is preserved in `tauri dev`. The `get_debug_logs` IPC command returns a snapshot; the frontend's `useDebugLogs` hook merges backend + frontend logs by timestamp in the DebugPanel. Frontend logging goes through `src/lib/logger.ts` (which mirrors to console in dev and buffers in memory).

### Auto-Update

The app uses `tauri-plugin-updater` backed by GitHub releases. On launch, `useUpdater` checks `latest.json` on the releases page; if a newer version exists, a persistent toast with "Download & Install" appears. The release workflow signs artifacts with `TAURI_SIGNING_PRIVATE_KEY` and generates the `latest.json` manifest via `includeUpdaterJson: true`. The public key is in `tauri.conf.json` under `plugins.updater.pubkey`.

### Send-Failure Toasts

The `ToastProvider` (src/components/ToastProvider.tsx) surfaces send/accept failures as ephemeral toasts (auto-dismiss 5s, max 3 visible). DropZone fires a toast on empty drops (e.g. Recycle Bin). `useTailscale` accepts an `onSendError` callback that App.tsx wires to `toast.error()`.

## TypeScript Strictness

`tsconfig.json` enforces `strict: true`, `noUnusedLocals`, `noUnusedParameters`, and `noFallthroughCasesInSwitch`. All code must pass `tsc --noEmit`.

## CI

GitHub Actions matrix builds on Ubuntu 22.04, macOS latest, and Windows latest. Release workflow (`v*` tags) produces signed macOS universal binaries, Windows MSI, and Linux AppImage/deb.
