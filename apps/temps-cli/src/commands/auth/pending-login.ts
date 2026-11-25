// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Browser sign-ins that were started with `temps login --no-wait` and not yet
 * finished with `temps login --resume`.
 *
 * A caller that cannot hold a command open while a person approves in the
 * browser (an agent whose shell tool returns output only when the command
 * exits) needs the link first and the wait second, as two commands. The
 * device code that links them is kept here between the two.
 *
 * Storage: `~/.temps/.pending-logins.json`, mode 0600, beside the contexts
 * file and for the same reason: a device code is a bearer secret until its
 * session expires, since whoever polls with it receives the API key.
 */

import { readFile, writeFile, mkdir, unlink } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { dirname } from 'node:path'

export interface PendingLogin {
  /** Context the credentials will be saved under. */
  context: string
  /** `/api`-suffixed server URL the session was started against. */
  apiBaseUrl: string
  deviceCode: string
  userCode: string
  /** Absolute URL the person opens to approve. */
  verificationUrl: string
  /** Polling interval the server advertised, in seconds. */
  intervalSecs: number
  /** ISO 8601 time the server stops accepting this device code. */
  expiresAt: string
}

export function pendingLoginsPath(): string {
  const home = process.env.HOME || process.env.USERPROFILE || '~'
  return `${home}/.temps/.pending-logins.json`
}

function isPendingLogin(value: unknown): value is PendingLogin {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Record<string, unknown>
  return (
    typeof v.context === 'string' &&
    typeof v.apiBaseUrl === 'string' &&
    typeof v.deviceCode === 'string' &&
    typeof v.userCode === 'string' &&
    typeof v.verificationUrl === 'string' &&
    typeof v.intervalSecs === 'number' &&
    typeof v.expiresAt === 'string'
  )
}

async function loadAll(): Promise<PendingLogin[]> {
  const path = pendingLoginsPath()
  try {
    if (existsSync(path)) {
      const parsed = JSON.parse(await readFile(path, 'utf-8')) as unknown
      if (Array.isArray(parsed)) return parsed.filter(isPendingLogin)
    }
  } catch {
    // Missing or malformed: nothing is pending.
  }
  return []
}

async function saveAll(entries: PendingLogin[]): Promise<void> {
  const path = pendingLoginsPath()
  if (entries.length === 0) {
    // Never leave an empty 0600 husk behind.
    if (existsSync(path)) {
      try {
        await unlink(path)
      } catch {
        // Best effort.
      }
    }
    return
  }
  await mkdir(dirname(path), { recursive: true })
  await writeFile(path, JSON.stringify(entries, null, 2) + '\n', { mode: 0o600 })
}

/** Whether the server would still accept this device code at `now`. */
export function pendingLoginExpired(entry: PendingLogin, now: number): boolean {
  const expires = Date.parse(entry.expiresAt)
  return Number.isNaN(expires) || expires <= now
}

/**
 * Remember a started sign-in. One per context: starting again replaces the
 * earlier one, whose link the person may never have opened. Expired entries
 * for other contexts are dropped on the way, so the file cannot grow.
 */
export async function savePendingLogin(
  entry: PendingLogin,
  now: number = Date.now(),
): Promise<void> {
  const others = (await loadAll()).filter(
    (e) => e.context !== entry.context && !pendingLoginExpired(e, now),
  )
  await saveAll([...others, entry])
}

/** The started sign-in for `context`, expired or not, or null. */
export async function getPendingLogin(context: string): Promise<PendingLogin | null> {
  return (await loadAll()).find((e) => e.context === context) ?? null
}

/** Forget the started sign-in for `context`. */
export async function clearPendingLogin(context: string): Promise<void> {
  const all = await loadAll()
  const rest = all.filter((e) => e.context !== context)
  if (rest.length !== all.length) await saveAll(rest)
}
