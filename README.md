# TailDrop

A drag-and-drop file transfer desktop app for Tailscale Taildrop. Built with Tauri 2.0, React 19, and TypeScript.

## Features

- **Node Discovery** — auto-discovers all Tailscale peers with online/offline status, sorted alphabetically
- **Drag & Drop Sending** — drop files onto a node card or the drop zone to send via Taildrop
- **File Receiving** — polls for incoming files with accept/save workflow
- **Auto-Accept** — optionally auto-accept incoming files to a configured directory
- **Transfer History** — shows all sent/received files with timestamps and status
- **Settings** — hide nodes, set default save directory, toggle offline/exit node visibility

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
| **Linux** | LocalAPI (Unix socket) | Connects to `/var/run/tailscale/tailscaled.sock` via `hyperlocal` + `hyper`. Uses peer stable node ID for file transfers. |
| **macOS** | CLI (`tailscale`) | Invokes the `tailscale` binary directly (avoids socket permission issues with signed .app bundles). Uses peer hostname for file transfers. |
| **Windows** | CLI (`tailscale.exe`) | Invokes `tailscale.exe` with `CREATE_NO_WINDOW` flag. Uses peer hostname for file transfers. |

**Linux note:** You may need elevated permissions or group membership to access the Tailscale socket.

## CI

GitHub Actions runs debug builds on Ubuntu 22.04, macOS latest, and Windows latest. The release workflow (`v*` tags) produces platform installers via `tauri-apps/tauri-action`.

## License

MIT
