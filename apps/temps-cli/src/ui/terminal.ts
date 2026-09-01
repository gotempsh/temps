// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Make untrusted text safe for human-readable terminal output.
 *
 * JSON output must not use this helper: JSON.stringify already escapes control
 * characters and callers expect the original data. This is for strings that
 * will be written directly to a terminal, where ANSI/OSC sequences can alter
 * the clipboard, links, title, colours, cursor position, or visible history.
 */
export function sanitizeTerminalText(value: unknown): string {
  let text = String(value)

  // OSC sequences end with BEL or ST. Handle both 7-bit (ESC ]) and 8-bit
  // (C1 OSC) forms, including an unterminated sequence at end-of-input.
  text = text.replace(
    /\x1B\](?:[^\x07\x1B]|\x1B(?!\\))*(?:\x07|\x1B\\|$)/g,
    '',
  )
  text = text.replace(/\x9D[^\x07\x9C]*(?:\x07|\x9C|$)/g, '')

  // CSI covers colour, cursor movement, erasure, and terminal mode changes.
  text = text.replace(/(?:\x1B\[|\x9B)[0-?]*[ -/]*[@-~]/g, '')

  // Strip remaining two-byte/intermediate ESC sequences and lone ESC bytes.
  text = text.replace(/\x1B[ -/]*[@-~]?/g, '')

  // A value must never create a second terminal line. Preserve word separation
  // while removing the remaining C0/C1 controls and DEL.
  text = text.replace(/\r\n?|\n|\t/g, ' ')
  text = text.replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F-\x9F]/g, '')

  // Bidirectional marks, isolates, and overrides can visually reorder commands
  // and identifiers even though they do not alter the underlying byte order.
  return text.replace(/[\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/g, '')
}
