// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFileSync } from 'node:child_process'

/** Resolves a binary on PATH without invoking a shell (avoids injection entirely). */
export function resolveOnPath(bin: string): string | null {
  try {
    const finder = process.platform === 'win32' ? 'where' : 'which'
    const out = execFileSync(finder, [bin], { stdio: ['ignore', 'pipe', 'ignore'] }).toString().trim()
    return out.split(/\r?\n/)[0] || null
  } catch {
    return null
  }
}

/** Extracts stderr from a failed execFileSync call, falling back to the error message. */
export function execErrorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'stderr' in error) {
    const stderr = (error as { stderr?: unknown }).stderr
    if (stderr) return Buffer.isBuffer(stderr) ? stderr.toString() : String(stderr)
  }
  return error instanceof Error ? error.message : String(error)
}
