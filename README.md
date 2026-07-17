# TailDrop

A drag-and-drop file transfer desktop app for Tailscale Taildrop. Built with Tauri 2.0, React 19, and TypeScript.

## Features

- **Node Discovery** — auto-discovers all Tailscale peers with online/offline status, sorted alphabetically
- **Drag & Drop Sending** — drop files onto a node card or the drop zone to send via Taildrop
- **File Receiving** — polls for incoming files with accept/save workflow (all platforms; macOS and Windows auto-receive via CLI fallback when socket/pipe is unavailable)
- **Auto-Accept** — optionally auto-accept incoming files to a configured directory
- **Desktop Notifications** — opt-in native notifications when files arrive
- **Transfer History** — persistent history of sent/received files with timestamps and status (survives app restarts)
- **Search** — filter nodes by name, hostname, or IP in the sidebar and settings
- **Settings** — hide nodes, browse for save directory, auto-accept, start on boot, toggle offline/exit node visibility
- **Pretty Names** — displays Tailscale machine names with title case (e.g. `pixel-10-pro-xl` → `Pixel 10 Pro XL`)
- **Exit Node Filtering** — Mullvad and other exit nodes from external tailnets are hidden by default, togglable in settings
- **File Safety** — overwrite protection (auto-renames duplicates), path traversal prevention, Content Security Policy enabled

## Prerequisites

### All Platforms
- [Node.js](https://nodejs.org/) 18+
- [Rust](https://rustup.rs/) 1.70+
- [Tailscale](https://tailscale.com/) installed and running

### macOS
```bash
xcode-select --install
```

### Windows
- Microsoft Visual Studio C++ Build Tools
- WebView2 (pre-installed on Windows 10 21H2+ and Windows 11)

### Linux
```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

## Build & Run

```bash
# Install frontend dependencies
npm install

# Run in development mode
npm run tauri dev

# Build for production
npm run tauri build
```

The production build output will be in `src-tauri/target/release/bundle/`.

## Architecture

```
taildrop-gui/
├── src/                    # React frontend
│   ├── components/         # UI components (Sidebar, DropZone, TransferHistory, Settings, DebugPanel, ToastProvider)
│   ├── hooks/              # useTailscale, useUpdater, useDebugLogs (state management + Tauri IPC)
│   ├── lib/                # logger.ts, toErrorMsg.ts (shared utils)
│   └── types/              # TypeScript interfaces
├── src-tauri/              # Rust backend
│   └── src/
│       ├── lib.rs          # Tauri commands (IPC bridge)
│       ├── tailscale.rs    # Platform-specific Tailscale communication
│       ├── debug_log.rs    # In-memory log sink for DebugPanel
│       └── main.rs         # Entry point
```

## Platform Backends

`tailscale.rs` contains three platform implementations behind `#[cfg]` gates:

| Platform | Method | Details |
|----------|--------|---------|
| **Linux** | LocalAPI (Unix socket) | Connects to `/var/run/tailscale/tailscaled.sock` via `hyperlocal` + `hyper`. Uses peer stable node ID for file transfers. Full incoming file support. |
| **macOS** | Socket-first, CLI fallback | Tries Unix socket for all operations; falls back to `tailscale` CLI when socket is unavailable (App Store installs). Incoming files are auto-received to the save directory via CLI when the socket is inaccessible. |
| **Windows** | Named pipe, CLI fallback | Tries the Tailscale named pipe for incoming file detection; falls back to `tailscale.exe file get` CLI auto-receive when the pipe is inaccessible (non-admin). Sends via CLI with `CREATE_NO_WINDOW`. |

**Linux note:** Run `sudo tailscale set --operator=$USER` once to grant your user access to the Tailscale socket without needing root.

**macOS note:** The macOS GUI Tailscale daemon uses a TCP loopback port instead of a Unix socket, so the socket path is usually unavailable. Incoming files are auto-downloaded to your save directory via the CLI fallback.

**Windows note:** The named pipe requires admin privileges. Without elevation, incoming files are auto-downloaded to your save directory via the CLI fallback.

## CI

GitHub Actions runs debug builds on Ubuntu 22.04, macOS latest, and Windows latest. The release workflow (`v*` tags) produces platform installers via `tauri-apps/tauri-action`.

## License

MIT
