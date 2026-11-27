// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The keys `mcp add` minted on this computer, one per app install (an app
 * and the settings it was written to). `mcp add` and `mcp remove` delete a
 * key only when it is the one recorded here for that install and the
 * install's entry still sends it, or, for `mcp add`, when an install into the
 * same settings that ran at the same time recorded it and settled it, and
 * this one replaced its entry: never a key found by name or prefix, another
 * profile's or another computer's, or one the user supplied.
 *
 * Only a digest of each key is kept; the key itself stays in the app's
 * settings and nowhere else.
 */

import { createHash, randomBytes } from 'node:crypto'
import * as fs from 'node:fs'
import * as os from 'node:os'
import * as path from 'node:path'

/** A key `mcp add` minted for one install. */
export interface MintedKey {
  /** Origin of the Temps instance the key belongs to. */
  origin: string
  /** The key's ID on that instance. */
  id: number
  name: string
  /** SHA-256 of the key, to tell whether an entry still sends it. */
  sha256: string
  /**
   * Set once its install has written its entry and read it back sending
   * this key: from then on, an entry that sends another key was replaced
   * after it, never before.
   */
  settled?: boolean
}

/** Most installs remembered; the oldest are forgotten first. */
const MAX_ENTRIES = 200
/** Most bytes read from one record, far more than one needs. */
const MAX_RECORD_BYTES = 16 * 1024

/**
 * The ledger: a 0700 directory with one 0600 file per key minted for an
 * install, named by the install and the key's digest. No two `mcp` commands
 * write the same file (each mints its own key), so none can drop another's
 * record, even two installs into the same settings at once, and no lock is
 * needed: a record is written whole and moved into place.
 */
export function ledgerPath(home: string = os.homedir()): string {
  return path.join(home, '.temps', 'mcp', 'minted-keys')
}

export function keyDigest(apiKey: string): string {
  return createHash('sha256').update(apiKey.trim()).digest('hex')
}

/** The install an entry belongs to: the app and where its settings are. */
export function installScope(clientId: string, target: string): string {
  return `${clientId}\n${target}`
}

function isMintedKey(value: unknown): value is MintedKey {
  if (!value || typeof value !== 'object') return false
  const { origin, id, name, sha256 } = value as Record<string, unknown>
  return (
    typeof origin === 'string' &&
    Number.isSafeInteger(id) &&
    typeof name === 'string' &&
    typeof sha256 === 'string' &&
    /^[0-9a-f]{64}$/.test(sha256)
  )
}

const RECORD_NAME = /^[0-9a-f]{64}-[0-9a-f]{64}\.json$/

function scopeDigest(scope: string): string {
  return createHash('sha256').update(scope).digest('hex')
}

/** The file recording the key with digest `sha256` for `scope`. */
function recordFile(dir: string, scope: string, sha256: string): string {
  return path.join(dir, `${scopeDigest(scope)}-${sha256}.json`)
}

/** A record file, or `undefined` when it is missing, too large, or not `scope`'s key it is named for. */
async function readRecord(file: string, scope: string): Promise<MintedKey | undefined> {
  try {
    const stat = await fs.promises.stat(file)
    if (!stat.isFile() || stat.size > MAX_RECORD_BYTES) return undefined
    const parsed: unknown = JSON.parse(await fs.promises.readFile(file, 'utf8'))
    if (!parsed || typeof parsed !== 'object' || (parsed as { scope?: unknown }).scope !== scope) return undefined
    const { origin, id, name, sha256, settled } = parsed as MintedKey
    const key: MintedKey = { origin, id, name, sha256, ...(settled === true ? { settled } : {}) }
    return isMintedKey(key) && path.basename(file) === `${scopeDigest(scope)}-${sha256}.json` ? key : undefined
  } catch {
    return undefined
  }
}

async function ensureDir(dir: string): Promise<void> {
  await fs.promises.mkdir(dir, { recursive: true, mode: 0o700 })
  await fs.promises.chmod(dir, 0o700)
}

/** Forget the oldest records past MAX_ENTRIES; a failure here loses nothing needed now. */
async function prune(dir: string, keep: string): Promise<void> {
  try {
    const names = (await fs.promises.readdir(dir)).filter((name) => RECORD_NAME.test(name))
    if (names.length <= MAX_ENTRIES) return
    const others = names.filter((name) => name !== path.basename(keep))
    const dated = await Promise.all(
      others.map(async (name) => {
        const stat = await fs.promises.stat(path.join(dir, name)).catch(() => null)
        return { name, at: stat?.mtimeMs ?? 0 }
      }),
    )
    dated.sort((a, b) => a.at - b.at)
    for (const { name } of dated.slice(0, names.length - MAX_ENTRIES)) {
      await fs.promises.rm(path.join(dir, name), { force: true })
    }
  } catch {
    // Best effort.
  }
}

/**
 * The record of `installed` (the key an install's entry sends) when `mcp
 * add` minted it for `scope` on this computer; `undefined` for any other key.
 */
export async function mintedKeyFor(
  scope: string,
  installed: string | undefined,
  dir: string = ledgerPath(),
): Promise<MintedKey | undefined> {
  if (!installed) return undefined
  return readRecord(recordFile(dir, scope, keyDigest(installed)), scope)
}

/**
 * The digests of the keys recorded for `scope`, read from the ledger's file
 * names alone. Throws when the ledger exists but cannot be listed, so a
 * caller never takes an unreadable ledger for an empty one.
 */
export async function recordedDigests(scope: string, dir: string = ledgerPath()): Promise<Set<string>> {
  const prefix = `${scopeDigest(scope)}-`
  let names: string[]
  try {
    names = await fs.promises.readdir(dir)
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return new Set()
    throw error
  }
  return new Set(
    names
      .filter((name) => name.startsWith(prefix) && RECORD_NAME.test(name))
      .map((name) => name.slice(prefix.length, -'.json'.length)),
  )
}

/**
 * Every key `mcp add` minted for `scope` and still records: more than one
 * only after two installs into the same settings ran at once, or a deletion
 * failed.
 */
export async function mintedKeysFor(scope: string, dir: string = ledgerPath()): Promise<MintedKey[]> {
  const prefix = `${scopeDigest(scope)}-`
  let names: string[]
  try {
    names = (await fs.promises.readdir(dir)).filter((name) => name.startsWith(prefix) && RECORD_NAME.test(name))
  } catch {
    return []
  }
  const records = await Promise.all(names.map((name) => readRecord(path.join(dir, name), scope)))
  return records.filter((record): record is MintedKey => record !== undefined)
}

/**
 * Whether `installed` (the key an install's entry sends) is the key
 * recorded for it on `origin`: only then is it one `mcp add` minted.
 */
export function isRecordedKey(record: MintedKey | undefined, installed: string | undefined, origin: string | null) {
  return Boolean(record && installed && origin && record.origin === origin && record.sha256 === keyDigest(installed))
}

/** Record a key `mcp add` minted for `scope`, or mark its record settled. */
export async function recordMintedKey(scope: string, key: MintedKey, dir: string = ledgerPath()): Promise<void> {
  await ensureDir(dir)
  const file = recordFile(dir, scope, key.sha256)
  const record = {
    scope,
    origin: key.origin,
    id: key.id,
    name: key.name,
    sha256: key.sha256,
    ...(key.settled ? { settled: true } : {}),
  }
  const temp = `${file}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`
  try {
    await fs.promises.writeFile(temp, `${JSON.stringify(record, null, 2)}\n`, {
      mode: 0o600,
      flag: 'wx',
    })
    await fs.promises.rename(temp, file)
  } catch (error) {
    await fs.promises.rm(temp, { force: true })
    throw error
  }
  await prune(dir, file)
}

/** Forget the key with digest `sha256` recorded for `scope`. */
export async function forgetMintedKey(scope: string, sha256: string, dir: string = ledgerPath()): Promise<void> {
  await fs.promises.rm(recordFile(dir, scope, sha256), { force: true })
}
