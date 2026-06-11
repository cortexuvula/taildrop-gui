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

Frontend calls `invoke("command_name", { camelCaseArgs })` which Tauri auto-converts to `snake_case` Rust parameters. All IPC commands are defined in `src-tauri/src/lib.rs` and delegate to `src-tauri/src/tailscale.rs`.

### Platform-Specific Rust Modules

`tailscale.rs` contains three `mod platform` blocks behind `#[cfg]` gates:

- **Linux** (`cfg(all(unix, not(target_os = "macos")))`): Communicates via Unix socket at `/var/run/tailscale/tailscaled.sock` using `hyperlocal` + `hyper`. Uses the Tailscale localapi directly (HTTP GET/PUT/DELETE).
- **macOS** (`cfg(target_os = "macos")`): Uses the `tailscale` CLI binary (avoids socket permission issues with signed .app bundles). Finds binary from known paths.
- **Windows** (`cfg(windows)`): Uses `tailscale.exe` CLI with `CREATE_NO_WINDOW` flag. Finds binary from Program Files paths.

All three expose the same public async functions: `fetch_status_json`, `send_file`, `get_incoming_files`, `accept_file`. macOS/Windows wrap blocking `Command::output()` in `tokio::task::spawn_blocking`.

**Key difference**: Linux localapi needs the peer's stable node ID (`peer_id`), while macOS/Windows CLI uses hostname (`peer_name`). Both are passed through the IPC; each platform uses what it needs.

### Frontend State Management

`src/hooks/useTailscale.ts` is the single state hook — manages peers, transfers, settings, and polling. Peers refresh every 10s, incoming files every 5s. Settings persist to `localStorage`.

Peer filtering logic: only shows peers on the same tailnet (derived from self node's DNS name), with toggles for offline nodes and Mullvad/exit nodes.

### File Transfer Flow

Files are sent by **path** (not content). DropZone uses Tauri's native `onDragDropEvent` for drag-and-drop (returns file paths) and `@tauri-apps/plugin-dialog` `open()` for browse. Rust reads the file from disk — on macOS/Windows it passes the path directly to `tailscale file cp`, on Linux it reads bytes and PUTs to the localapi.

## TypeScript Strictness

`tsconfig.json` enforces `strict: true`, `noUnusedLocals`, `noUnusedParameters`, and `noFallthroughCasesInSwitch`. All code must pass `tsc --noEmit`.

## CI

GitHub Actions matrix builds on Ubuntu 22.04, macOS latest, and Windows latest. Release workflow (`v*` tags) produces signed macOS universal binaries, Windows MSI, and Linux AppImage/deb.
