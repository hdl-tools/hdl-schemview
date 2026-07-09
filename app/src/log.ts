// Pure helpers for the status / log pane (#100, epic #94 4c). DOM-free so they
// unit-test under Vitest like tree.ts / wave.ts; main.ts renders the entries.

export type LogLevel = "info" | "warn" | "error";

export interface LogEntry {
  ts: string;
  level: LogLevel;
  message: string;
}

const pad2 = (n: number): string => String(n).padStart(2, "0");

/** A local wall-clock time as a zero-padded 24-hour `HH:MM:SS` stamp. */
export function formatTime(at: Date): string {
  return `${pad2(at.getHours())}:${pad2(at.getMinutes())}:${pad2(at.getSeconds())}`;
}

/** A structured log entry: timestamp string + level + message, ready to render. */
export function formatLogEntry(
  level: LogLevel,
  message: string,
  at: Date,
): LogEntry {
  return { ts: formatTime(at), level, message };
}
