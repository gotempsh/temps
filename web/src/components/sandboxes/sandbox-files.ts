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

/**
 * Whether a stat result is a regular file small enough to load into the
 * editor. It gates `fs/read`: nothing else may ask for the contents.
 */
export function isEditableFile(
  stat: { exists: boolean; is_file: boolean; size: number } | undefined
): boolean {
  return (
    !!stat && stat.exists && stat.is_file && stat.size <= MAX_EDITABLE_BYTES
  )
}

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

/** Mode a file is created with when none is sent (the API's own default). */
export const DEFAULT_FILE_MODE = 0o644

/**
 * Where the sandbox hands uploaded files to the sandbox user. Anywhere else
 * `fs/write` creates them as `root:root` (see `runtime_user_owns_uploaded_file`
 * in the sandbox provider), which the console cannot undo: the sandbox user
 * cannot `chown` a root-owned file back.
 */
export const SANDBOX_HOME = '/home/temps'
const SANDBOX_USER_UID = 1000

/** What the console needs to know about an existing file before rewriting it. */
export interface FileProperties {
  /** Permission bits including setuid/setgid/sticky, as `stat` reports them. */
  mode: number
  /** Numeric owner. */
  uid: number
  /** The path is a symbolic link (the mode and owner are the target's). */
  symlink: boolean
}

/**
 * Command that prints `<octal mode> <uid>` for a file, then `symlink` when the
 * path is a link. `fs/stat` carries neither, and `fs/write` resets a file to
 * 0644 unless a mode is sent, so the console reads them first.
 *
 * `-L` reports the link's target: without it a symlink's own mode is always
 * 777, which would then be written onto the saved file. The path travels as
 * `$1`, never inside the script text.
 */
export function filePropertiesArgv(path: string): string[] {
  return [
    'sh',
    '-c',
    'stat -L -c "%a %u" -- "$1" && if [ -L "$1" ]; then echo symlink; fi',
    'sh',
    path,
  ]
}

/** Parse the output of {@link filePropertiesArgv}; `null` when it is not. */
export function parseFileProperties(stdout: string): FileProperties | null {
  const [first, second] = stdout.trim().split('\n')
  const match = /^([0-7]{3,4}) (\d+)$/.exec((first ?? '').trim())
  if (!match) return null
  return {
    mode: parseInt(match[1]!, 8),
    uid: Number(match[2]),
    symlink: (second ?? '').trim() === 'symlink',
  }
}

/** The permission bits the console writes back: the rwx bits only. */
export function writeMode(properties: FileProperties | null): number {
  return properties ? properties.mode & 0o777 : DEFAULT_FILE_MODE
}

/**
 * True when the provider hands `path` to the sandbox user. This mirrors
 * `runtime_user_owns_uploaded_file` (a plain prefix test on `/home/temps`, so
 * `/home/temps-other/x` counts too): the console warns about what the provider
 * will really do, not about what a stricter rule would have done.
 */
export function isUnderSandboxHome(path: string): boolean {
  return path.startsWith(SANDBOX_HOME)
}

/** The numeric owner `fs/write` gives `path`, whatever owned it before. */
export function ownerAfterWrite(path: string): number {
  return isUnderSandboxHome(path) ? SANDBOX_USER_UID : 0
}

/**
 * What saving would change besides the text, for an existing file. Empty means
 * the save is faithful and needs no confirmation.
 */
export function saveRisks(
  path: string,
  properties: FileProperties | null
): string[] {
  if (!properties) {
    return [
      'Its permissions could not be read, so it will be reset to 0644 (rw-r--r--).',
    ]
  }
  const risks: string[] = []
  if (properties.symlink) {
    risks.push(
      'This path is a symbolic link. Saving replaces the link with a regular file holding your text; the file it pointed to is not changed.'
    )
  }
  // Every write re-creates the file, so its owner becomes whoever the provider
  // writes as for this path -- including inside the home, where a file the
  // sandbox user did not own (a root-owned 0644, say) is handed to uid 1000.
  const writtenUid = ownerAfterWrite(path)
  if (properties.uid !== writtenUid) {
    risks.push(
      writtenUid === 0
        ? `Outside ${SANDBOX_HOME} the sandbox writes files as root, so the sandbox user may lose access to it (this matters most for 0600 files).`
        : `Saving makes the sandbox user (uid ${writtenUid}) the owner of this file; it is currently owned by uid ${properties.uid}.`
    )
  }
  if ((properties.mode & ~0o777) !== 0) {
    risks.push(
      'Its setuid, setgid or sticky bit is not kept; only the rwx permissions are written back.'
    )
  }
  return risks
}

/** Properties a file has right after the console wrote it (for the cache). */
export function propertiesAfterSave(
  path: string,
  mode: number
): FileProperties {
  return {
    mode,
    uid: ownerAfterWrite(path),
    symlink: false,
  }
}

/** `0o755` -> `"0755"`, as `ls`/`chmod` users write it. */
export function formatFileMode(mode: number): string {
  return mode.toString(8).padStart(4, '0')
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
