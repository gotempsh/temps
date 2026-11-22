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

/** The directory the browser starts in: the work dir, else the root. */
export function initialDirectory(workDir: string | null | undefined): string {
  const dir = (workDir ?? '').trim().replace(/\/+$/, '')
  return dir.startsWith('/') ? dir : '/'
}

/** The directory containing `path`; the root is its own parent. */
export function parentDirectory(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  const cut = trimmed.lastIndexOf('/')
  return cut <= 0 ? '/' : trimmed.slice(0, cut)
}

/** `name` inside `directory`, without doubling the separator at the root. */
export function joinSandboxPath(directory: string, name: string): string {
  return directory === '/' || directory === ''
    ? `/${name}`
    : `${directory.replace(/\/+$/, '')}/${name}`
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
 * True when the Docker provider's tar upload hands `path` to the sandbox user.
 * This is the prefix rule of `runtime_user_owns_uploaded_file` (a plain
 * `starts_with` on `/home/temps`, so `/home/temps-other/x` counts too): the
 * console warns about what the provider really does, not about what a
 * stricter rule would have done. The platform's own `/run/secrets/temps-chat-*`
 * files take a different write path and are not modelled here.
 */
export function isUnderSandboxHome(path: string): boolean {
  return path.startsWith(SANDBOX_HOME)
}

/** The numeric owner a Docker `fs/write` gives `path`, whoever owned it before. */
export function ownerAfterWrite(path: string): number {
  return isUnderSandboxHome(path) ? SANDBOX_USER_UID : 0
}

/**
 * What `fs/write` does to a file that already exists, which depends on the
 * sandbox backend and decides which save warnings are true.
 *
 * - `recreate`: the file is replaced by a fresh one (Docker uploads a tar), so
 *   a symlink at the path is replaced and the owner is set from the path.
 * - `in-place`: the existing file is written and chmod'd where it is
 *   (Firecracker's VM agent and the local backend use `std::fs::write` and
 *   `set_permissions`), so a symlink is written through and the owner stays.
 * - `unknown`: not documented for this backend (microsandbox) or not recorded
 *   (legacy rows). Warnings are then worded as possibilities, not facts.
 */
export type WriteSemantics = 'recreate' | 'in-place' | 'unknown'

export function writeSemantics(
  backend: string | null | undefined
): WriteSemantics {
  switch (backend) {
    case 'docker':
      return 'recreate'
    case 'firecracker':
    case 'local':
      return 'in-place'
    default:
      return 'unknown'
  }
}

/**
 * What saving would change besides the text, for an existing file. Empty means
 * the save is faithful and needs no confirmation. Only consequences that are
 * true for `backend` are reported: an in-place backend never replaces a link or
 * changes an owner, and an unknown one is described in terms of "may".
 */
export function saveRisks(
  path: string,
  properties: FileProperties | null,
  backend: string | null | undefined
): string[] {
  if (!properties) {
    return [
      'Its permissions could not be read, so it will be reset to 0644 (rw-r--r--).',
    ]
  }
  const semantics = writeSemantics(backend)
  const risks: string[] = []
  if (properties.symlink && semantics !== 'in-place') {
    risks.push(
      semantics === 'recreate'
        ? 'This path is a symbolic link. Saving replaces the link with a regular file holding your text; the file it pointed to is not changed.'
        : 'This path is a symbolic link. Depending on the sandbox backend, saving either replaces the link with a regular file or writes through it to the file it points to.'
    )
  }
  // A recreating write gives the file whoever the provider writes as for this
  // path -- inside the home that is the sandbox user, so a root-owned 0644
  // there is handed over; elsewhere it is root.
  const writtenUid = ownerAfterWrite(path)
  if (semantics !== 'in-place' && properties.uid !== writtenUid) {
    if (semantics === 'unknown') {
      risks.push(
        `Saving may change who owns this file: it is owned by uid ${properties.uid} and this sandbox backend might write it as uid ${writtenUid}.`
      )
    } else if (writtenUid === 0) {
      risks.push(
        `Outside ${SANDBOX_HOME} the sandbox writes files as root, so the sandbox user may lose access to it (this matters most for 0600 files).`
      )
    } else {
      risks.push(
        `Saving makes the sandbox user (uid ${writtenUid}) the owner of this file; it is currently owned by uid ${properties.uid}.`
      )
    }
  }
  // Every backend writes the mode explicitly, rwx bits only.
  if ((properties.mode & ~0o777) !== 0) {
    risks.push(
      'Its setuid, setgid or sticky bit is not kept; only the rwx permissions are written back.'
    )
  }
  return risks
}

/**
 * Properties a file has right after the console wrote it (for the cache).
 * Decisions are not made from this -- Save probes the file again -- but the
 * cache should not claim an owner the write did not produce: an in-place write
 * leaves the previous owner and link status as they were.
 */
export function propertiesAfterSave(
  path: string,
  mode: number,
  backend: string | null | undefined,
  previous: FileProperties | null
): FileProperties {
  if (previous && writeSemantics(backend) !== 'recreate') {
    return { mode, uid: previous.uid, symlink: previous.symlink }
  }
  return { mode, uid: ownerAfterWrite(path), symlink: false }
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
