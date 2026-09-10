// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ClaudeCodeAdapter } from './claude-code.js'
import { ClaudeDesktopAdapter } from './claude-desktop.js'
import { CodexAdapter } from './codex.js'
import { CursorAdapter } from './cursor.js'
import { VsCodeAdapter } from './vscode.js'
import { WindsurfAdapter } from './windsurf.js'
import { ZedAdapter } from './zed.js'
import type { McpClientAdapter } from './base.js'

export const CLIENT_ADAPTERS: McpClientAdapter[] = [
  new ClaudeCodeAdapter(),
  new ClaudeDesktopAdapter(),
  new CodexAdapter(),
  new CursorAdapter(),
  new VsCodeAdapter(),
  new WindsurfAdapter(),
  new ZedAdapter(),
]

export function getClientAdapter(id: string): McpClientAdapter | undefined {
  return CLIENT_ADAPTERS.find((c) => c.id === id)
}

export function listClientIds(): string[] {
  return CLIENT_ADAPTERS.map((c) => c.id)
}

export type { InstallResult, McpClientAdapter, McpServerEntry } from './base.js'
