// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { chmodSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  forgetMintedKey,
  installScope,
  isRecordedKey,
  keyDigest,
  ledgerPath,
  mintedKeyFor,
  mintedKeysFor,
  recordedDigests,
  recordMintedKey,
} from './minted-keys.js'

const ORIGIN = 'https://temps.example.test'
const key = (id: number, secret: string) => ({
  origin: ORIGIN,
  id,
  name: `mcp-cursor-${id}`,
  sha256: keyDigest(secret),
})

describe('the minted-key ledger', () => {
  let home: string
  let file: string
  const only = () => {
    const names = readdirSync(file)
    expect(names.length).toBe(1)
    return join(file, names[0] ?? '')
  }
  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), 'temps-minted-'))
    file = ledgerPath(home)
  })
  afterEach(() => rmSync(home, { recursive: true, force: true }))

  test('records each key minted for an install, found by the key its entry sends, readable only by its owner', async () => {
    const here = installScope('cursor', 'settings A')
    const there = installScope('cursor', 'settings B')
    await recordMintedKey(here, key(1, 'tk_one_secret'), file)
    await recordMintedKey(there, key(2, 'tk_two_secret'), file)
    expect(await mintedKeyFor(here, 'tk_one_secret', file)).toEqual(key(1, 'tk_one_secret'))
    expect(await mintedKeyFor(there, 'tk_two_secret', file)).toEqual(key(2, 'tk_two_secret'))
    // Another install's key, one never recorded, or none: no record.
    expect(await mintedKeyFor(here, 'tk_two_secret', file)).toBeUndefined()
    expect(await mintedKeyFor(here, 'tk_users_own', file)).toBeUndefined()
    expect(await mintedKeyFor(here, undefined, file)).toBeUndefined()
    for (const name of readdirSync(file)) {
      expect(readFileSync(join(file, name), 'utf8')).not.toContain('tk_one_secret')
      expect(statSync(join(file, name)).mode & 0o777).toBe(0o600)
    }
    expect(statSync(file).mode & 0o777).toBe(0o700)

    await forgetMintedKey(here, keyDigest('tk_one_secret'), file)
    expect(await mintedKeyFor(here, 'tk_one_secret', file)).toBeUndefined()
    expect(await mintedKeyFor(there, 'tk_two_secret', file)).toEqual(key(2, 'tk_two_secret'))
  })

  test('a missing, broken or oversized record is none, and one for another install is ignored', async () => {
    const scope = installScope('cursor', 'settings A')
    expect(await mintedKeyFor(scope, 'tk_one', file)).toBeUndefined()
    await recordMintedKey(scope, key(1, 'tk_one'), file)
    const record = only()
    writeFileSync(record, '{not json')
    expect(await mintedKeyFor(scope, 'tk_one', file)).toBeUndefined()
    writeFileSync(
      record,
      JSON.stringify({
        scope,
        origin: ORIGIN,
        id: '1',
        name: 'n',
        sha256: 'abc',
      }),
    )
    expect(await mintedKeyFor(scope, 'tk_one', file)).toBeUndefined()
    writeFileSync(
      record,
      JSON.stringify({
        scope: installScope('cursor', 'settings B'),
        ...key(1, 'tk_one'),
      }),
    )
    expect(await mintedKeyFor(scope, 'tk_one', file)).toBeUndefined()
    writeFileSync(
      record,
      JSON.stringify({
        scope,
        ...key(1, 'tk_one'),
        pad: 'x'.repeat(20 * 1024),
      }),
    )
    expect(await mintedKeyFor(scope, 'tk_one', file)).toBeUndefined()
  })

  test('a key is the recorded one only with the same secret on the same instance', () => {
    const recorded = key(1, 'tk_one_secret')
    expect(isRecordedKey(recorded, 'tk_one_secret', ORIGIN)).toBe(true)
    expect(isRecordedKey(recorded, ' tk_one_secret\n', ORIGIN)).toBe(true)
    expect(isRecordedKey(recorded, 'tk_one_secreT', ORIGIN)).toBe(false)
    expect(isRecordedKey(recorded, 'tk_one_secret', 'http://temps.example.test')).toBe(false)
    expect(isRecordedKey(recorded, undefined, ORIGIN)).toBe(false)
    expect(isRecordedKey(undefined, 'tk_one_secret', ORIGIN)).toBe(false)
    expect(isRecordedKey(recorded, 'tk_one_secret', null)).toBe(false)
  })
})

describe('updating the ledger', () => {
  let home: string
  let file: string
  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), 'temps-minted-lock-'))
    file = ledgerPath(home)
  })
  afterEach(() => rmSync(home, { recursive: true, force: true }))

  test('installs recorded at the same time all keep their record', async () => {
    const scopes = Array.from({ length: 12 }, (_, i) => installScope(`client-${i}`, 'settings'))
    await Promise.all(scopes.map((scope, i) => recordMintedKey(scope, key(i + 1, `tk_${i}_secret`), file)))
    for (const [i, scope] of scopes.entries()) {
      expect(await mintedKeyFor(scope, `tk_${i}_secret`, file)).toEqual(key(i + 1, `tk_${i}_secret`))
    }
    expect(readdirSync(file).filter((name) => name.endsWith('.tmp'))).toEqual([])
  })

  test('separate processes recording at the same time all keep their record', async () => {
    const script = join(home, 'record.ts')
    writeFileSync(
      script,
      `import { recordMintedKey, keyDigest } from ${JSON.stringify(join(import.meta.dir, 'minted-keys.ts'))}
const [dir, i] = process.argv.slice(2)
await recordMintedKey('client-' + i + '\\nsettings', { origin: ${JSON.stringify(ORIGIN)}, id: Number(i) + 1, name: 'mcp-cursor-' + (Number(i) + 1), sha256: keyDigest('tk_' + i + '_secret') }, dir)
`,
    )
    const runs = Array.from({ length: 8 }, (_, i) => Bun.spawn([process.execPath, script, file, String(i)]))
    expect(await Promise.all(runs.map((run) => run.exited))).toEqual(Array(8).fill(0))
    for (let i = 0; i < 8; i++) {
      expect(await mintedKeyFor(installScope(`client-${i}`, 'settings'), `tk_${i}_secret`, file)).toEqual(
        key(i + 1, `tk_${i}_secret`),
      )
    }
  })

  test('past the limit the oldest installs are forgotten, the newest kept', async () => {
    for (let i = 0; i < 203; i++) await recordMintedKey(installScope(`c${i}`, 's'), key(i + 1, `tk_${i}`), file)
    expect(readdirSync(file).length).toBe(200)
    expect(await mintedKeyFor(installScope('c202', 's'), 'tk_202', file)).toEqual(key(203, 'tk_202'))
  })

  test('two installs into the same settings at once both keep their record', async () => {
    const scope = installScope('cursor', 'settings')
    await Promise.all([recordMintedKey(scope, key(1, 'tk_a'), file), recordMintedKey(scope, key(2, 'tk_b'), file)])
    expect(await mintedKeyFor(scope, 'tk_a', file)).toEqual(key(1, 'tk_a'))
    expect(await mintedKeyFor(scope, 'tk_b', file)).toEqual(key(2, 'tk_b'))
    expect((await mintedKeysFor(scope, file)).map((k) => k.id).sort()).toEqual([1, 2])
    expect(await mintedKeysFor(installScope('cursor', 'elsewhere'), file)).toEqual([])
  })

  test('the digests recorded for an install; a ledger that cannot be listed is an error, not empty', async () => {
    const scope = installScope('cursor', 'settings')
    expect(await recordedDigests(scope, file)).toEqual(new Set())
    await recordMintedKey(scope, key(1, 'tk_a'), file)
    await recordMintedKey(installScope('cursor', 'elsewhere'), key(2, 'tk_b'), file)
    expect(await recordedDigests(scope, file)).toEqual(new Set([keyDigest('tk_a')]))
    if (process.getuid?.() === 0) return // root lists any directory
    chmodSync(file, 0o300)
    try {
      await expect(recordedDigests(scope, file)).rejects.toThrow()
    } finally {
      chmodSync(file, 0o700)
    }
  })

  test('a record settled again replaces the first, and says so when read back', async () => {
    const scope = installScope('cursor', 'settings')
    await recordMintedKey(scope, key(1, 'tk_a'), file)
    expect(await mintedKeyFor(scope, 'tk_a', file)).toEqual(key(1, 'tk_a'))
    await recordMintedKey(scope, { ...key(1, 'tk_a'), settled: true }, file)
    expect(await mintedKeysFor(scope, file)).toEqual([{ ...key(1, 'tk_a'), settled: true }])
  })
})
