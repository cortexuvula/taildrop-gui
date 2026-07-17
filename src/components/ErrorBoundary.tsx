import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

/**
 * Top-level error boundary. Catches render-time exceptions that would
 * otherwise white-screen the entire app. Shows a minimal recovery UI with
 * the error message and a reload button.
 */
export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("[ErrorBoundary] Uncaught render error:", error, info.componentStack);
  }

  handleReload = (): void => {
    window.location.reload();
  };

  render(): ReactNode {
    if (this.state.hasError) {
      return (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            height: "100vh",
            background: "#0f1117",
            color: "#e4e6f0",
            fontFamily: "-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif",
            gap: 16,
            padding: 32,
            textAlign: "center",
          }}
        >
          <div style={{ fontSize: 48 }}>💥</div>
          <h1 style={{ fontSize: 20, fontWeight: 600 }}>Something went wrong</h1>
          <p style={{ color: "#8b8fa3", fontSize: 14, maxWidth: 420 }}>
            TailDrop hit an unexpected error. Try reloading — your transfer
            history and settings are preserved.
          </p>
          {this.state.error && (
            <pre
              style={{
                background: "#161822",
                border: "1px solid #2a2d3e",
                borderRadius: 8,
                padding: 12,
                fontSize: 12,
                maxWidth: 500,
                overflow: "auto",
                color: "#f87171",
                whiteSpace: "pre-wrap",
                wordBreak: "break-all",
              }}
            >
              {this.state.error.message}
            </pre>
          )}
          <button
            onClick={this.handleReload}
            style={{
              background: "#4f8ff7",
              color: "#fff",
              border: "none",
              borderRadius: 8,
              padding: "10px 24px",
              fontSize: 14,
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Reload app
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
