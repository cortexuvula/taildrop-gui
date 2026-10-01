// Mounts the PRODUCTION App (default export incl. ToastProvider) against the
// browser-side Tauri IPC mock. The mock import must come first so
// window.__TAURI_INTERNALS__ exists before any @tauri-apps/api call runs.
// StrictMode is intentionally omitted: its dev-mode double effect invocation
// adds noise without changing the behaviors under test.
import "./tauri-mock";
import { createRoot } from "react-dom/client";
import App from "../src/App";

createRoot(document.getElementById("root") as HTMLElement).render(<App />);
