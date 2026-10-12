// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Helpers for opening and saving one file in a sandbox through
 * `fs/read` and `fs/write`. Contents travel as base64 in both directions;
 * the console only edits UTF-8 text, so a file that is not valid UTF-8 (or is
 * too large to be worth a textarea) is described rather than corrupted.
 */

/** Largest file the console will load into the editor. */
export const MAX_EDITABLE_BYTES = 256 * 1024
const MAX_PATH_LENGTH = 4096

/** `null` when the path can be sent to the API, otherwise why it cannot. */
export function validateSandboxPath(raw: string): string | null {
  const path = raw.trim()
  if (path === '') return 'Enter the absolute path of a file.'
  if (!path.startsWith('/')) {
    return 'The path must be absolute, starting with /.'
  }
  if (path.endsWith('/')) return 'That is a directory; enter a file path.'
  if (path.includes('\0')) return 'The path contains an invalid character.'
  if (path.length > MAX_PATH_LENGTH) return 'The path is too long.'
  return null
}

/** The working directory as the start of a path the user can complete. */
export function defaultFilePath(workDir: string | null | undefined): string {
  const dir = (workDir ?? '').trim().replace(/\/+$/, '')
  return dir.startsWith('/') ? `${dir}/` : '/'
}

/** UTF-8 text to base64, without `btoa` choking on non-Latin-1 text. */
export function encodeText(text: string): string {
  const bytes = new TextEncoder().encode(text)
  let binary = ''
  const chunk = 0x8000
  for (let index = 0; index < bytes.length; index += chunk) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk))
  }
  return btoa(binary)
}

export type DecodedFile =
  { ok: true; text: string } | { ok: false; reason: 'binary' | 'invalid' }

/** Base64 file contents to text, refusing anything that is not UTF-8. */
export function decodeFile(contentsB64: string): DecodedFile {
  let binary: string
  try {
    binary = atob(contentsB64)
  } catch {
    return { ok: false, reason: 'invalid' }
  }
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0))
  if (bytes.includes(0)) return { ok: false, reason: 'binary' }
  try {
    return {
      ok: true,
      text: new TextDecoder('utf-8', { fatal: true }).decode(bytes),
    }
  } catch {
    return { ok: false, reason: 'binary' }
  }
}

/** Why the files panel cannot be used for this sandbox right now. */
export function filesUnavailableReason(sandbox: {
  status: string
  lifecycle?: string
}): string | null {
  if (sandbox.status === 'destroyed') return 'This sandbox has been destroyed.'
  if (sandbox.status === 'stopped' && sandbox.lifecycle !== 'workspace') {
    return 'This sandbox is stopped. Resume it to read or write files.'
  }
  return null
}
