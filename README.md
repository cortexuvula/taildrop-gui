# TailDrop

A drag-and-drop file transfer desktop app for Tailscale Taildrop. Built with Tauri 2.0, React 19, and TypeScript.

## Features

- **Node Discovery** — auto-discovers all Tailscale peers with online/offline status, sorted alphabetically
- **Drag & Drop Sending** — drop files onto a node card or the drop zone to send via Taildrop
- **File Receiving** — polls for incoming files with accept/save workflow (Linux; macOS when socket is accessible)
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
│   ├── components/         # UI components (Sidebar, DropZone, TransferHistory, Settings, DebugPanel)
│   ├── hooks/              # useTailscale hook (state management + Tauri IPC)
│   └── types/              # TypeScript interfaces
├── src-tauri/              # Rust backend
│   └── src/
│       ├── lib.rs          # Tauri commands (IPC bridge)
│       ├── tailscale.rs    # Platform-specific Tailscale communication
│       └── main.rs         # Entry point
```

## Platform Backends

`tailscale.rs` contains three platform implementations behind `#[cfg]` gates:

| Platform | Method | Details |
|----------|--------|---------|
| **Linux** | LocalAPI (Unix socket) | Connects to `/var/run/tailscale/tailscaled.sock` via `hyperlocal` + `hyper`. Uses peer stable node ID for file transfers. Full incoming file support. |
| **macOS** | CLI + socket fallback | Sends files via `tailscale` CLI (avoids socket permission issues with signed .app bundles). Attempts Unix socket for incoming file listing; falls back gracefully if inaccessible. |
| **Windows** | CLI (`tailscale.exe`) | Invokes `tailscale.exe` with `CREATE_NO_WINDOW` flag. Uses peer hostname for file transfers. Incoming file listing not yet supported (CLI limitation). |

**Linux note:** You may need elevated permissions or group membership to access the Tailscale socket.

**macOS note:** Incoming file detection works when the Tailscale Unix socket is accessible (e.g., Homebrew installs). App Store installs with restricted socket permissions will not detect incoming files.

## CI

GitHub Actions runs debug builds on Ubuntu 22.04, macOS latest, and Windows latest. The release workflow (`v*` tags) produces platform installers via `tauri-apps/tauri-action`.

## License

MIT
