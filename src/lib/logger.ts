export type LogLevel = "debug" | "info" | "warn" | "error";

export interface LogEntry {
  timestampMs: number;
  level: LogLevel;
  source: "frontend" | "backend";
  target: string;
  message: string;
}

const MAX_ENTRIES = 500;
const buffer: LogEntry[] = [];
const listeners = new Set<() => void>();

function safeStringify(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function push(level: LogLevel, target: string, message: string, ...rest: unknown[]) {
  const entry: LogEntry = {
    timestampMs: Date.now(),
    level,
    source: "frontend",
    target,
    message: rest.length
      ? `${message} ${rest.map(safeStringify).join(" ")}`
      : message,
  };
  buffer.push(entry);
  if (buffer.length > MAX_ENTRIES) buffer.shift();

  // Mirror to console in dev only (matches existing import.meta.env.DEV gating).
  if (import.meta.env.DEV) {
    const fn =
      level === "error" ? console.error
      : level === "warn" ? console.warn
      : level === "info" ? console.info
      : console.log;
    fn(`[${target}] ${message}`, ...rest);
  }

  listeners.forEach((l) => l());
}

export const logger = {
  debug: (target: string, message: string, ...rest: unknown[]) => push("debug", target, message, ...rest),
  info: (target: string, message: string, ...rest: unknown[]) => push("info", target, message, ...rest),
  warn: (target: string, message: string, ...rest: unknown[]) => push("warn", target, message, ...rest),
  error: (target: string, message: string, ...rest: unknown[]) => push("error", target, message, ...rest),
};

export function getFrontendLogs(): LogEntry[] {
  return [...buffer];
}

export function clearFrontendLogs(): void {
  buffer.length = 0;
  listeners.forEach((l) => l());
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
