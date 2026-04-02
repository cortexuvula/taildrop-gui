# TailDrop

A drag-and-drop file transfer desktop app for Tailscale Taildrop. Built with Tauri 2.0, React, and TypeScript.

## Features

- **Node Discovery** — auto-discovers all Tailscale peers with online/offline status
- **Drag & Drop Sending** — drop files onto a node card or the drop zone to send via Taildrop
- **File Receiving** — polls for incoming files with accept/save workflow
- **Transfer History** — shows all sent/received files with timestamps and status
- **Settings** — hide nodes, set default save directory, toggle auto-accept
- **System Tray** — runs in the background with incoming file notifications

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
│   ├── components/         # UI components (Sidebar, DropZone, TransferHistory, Settings)
│   ├── hooks/              # useTailscale hook (state management + Tauri IPC)
│   └── types/              # TypeScript interfaces
├── src-tauri/              # Rust backend
│   └── src/
│       ├── lib.rs          # Tauri commands (get_tailscale_status, send_file, etc.)
│       ├── tailscale.rs    # Tailscale LocalAPI client (Unix socket / named pipe)
│       └── main.rs         # Entry point
```

## Tailscale LocalAPI

The app communicates with the local Tailscale daemon over its Unix socket (`/var/run/tailscale/tailscaled.sock` on Linux/macOS) or named pipe (`\\.\pipe\ProtectedPrefix\Tailscale` on Windows).

**Note:** You may need to run with elevated permissions or ensure your user has access to the Tailscale socket.

## License

MIT
