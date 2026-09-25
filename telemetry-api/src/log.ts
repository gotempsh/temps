// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Structured JSON-lines logging: one JSON object per line with an explicit
// level and component, so the platform's log viewer can filter and parse it.
// Never pass a client IP here — only derived, anonymous values.

export type LogLevel = "debug" | "info" | "warn" | "error";

export function log(
  level: LogLevel,
  component: string,
  msg: string,
  fields: Record<string, unknown> = {}
): void {
  const line = JSON.stringify({
    ts: new Date().toISOString(),
    level,
    component,
    msg,
    ...fields,
  });
  if (level === "error" || level === "warn") console.error(line);
  else console.log(line);
}

// Fields describing a caught error, for `log(..., errorFields(err))`.
export function errorFields(err: unknown): Record<string, unknown> {
  if (err instanceof Error) {
    return { error: err.message, error_name: err.name, stack: err.stack };
  }
  return { error: String(err) };
}
