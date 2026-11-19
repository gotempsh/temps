// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { chmodSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const SCRIPT = join(import.meta.dir, 'approve-packages.sh')

let root: string
let tarball: string

/** A directory holding only the named host tools, plus any stub scripts given. */
function binDir(name: string, tools: string[], stubs: Record<string, string> = {}): string {
  const dir = join(root, name)
  mkdirSync(dir)
  for (const tool of tools) {
    const path = Bun.which(tool)
    if (!path) throw new Error(`${tool} is required to run these tests`)
    symlinkSync(path, join(dir, tool))
  }
  for (const [stub, body] of Object.entries(stubs)) {
    writeFileSync(join(dir, stub), `#!/bin/bash\n${body}\n`)
    chmodSync(join(dir, stub), 0o755)
  }
  return dir
}

/** Sources the script (which stops before its main flow) and runs `body` with PATH=`path`. */
function run(path: string, body: string): { code: number; stdout: string } {
  const bash = Bun.which('bash')
  if (!bash) throw new Error('bash is required to run these tests')
  const result = Bun.spawnSync([bash, '-c', `source "$SCRIPT" && ${body}`], {
    env: { PATH: path, SCRIPT, TGZ: tarball, WORK_DIR: join(root, 'work') },
  })
  return { code: result.exitCode, stdout: result.stdout.toString() }
}

beforeAll(() => {
  root = mkdtempSync(join(tmpdir(), 'approve-packages-test-'))
  tarball = join(root, 'temps-sdk-cli-0.1.38.tgz')
  writeFileSync(tarball, 'local tarball contents')
  mkdirSync(join(root, 'work'))
})

afterAll(() => {
  rmSync(root, { recursive: true, force: true })
})

describe('approve-packages.sh integrity', () => {
  test('matches the sha512 integrity string npm reports', () => {
    if (!Bun.which('openssl')) {
      console.log('openssl not available, skipping')
      return
    }
    const expected = `sha512-${createHash('sha512').update('local tarball contents').digest('base64')}`
    const { code, stdout } = run(binDir('real', ['openssl']), 'integrity "$TGZ"')
    expect(code).toBe(0)
    expect(stdout).toBe(expected)
  })

  test('fails and prints nothing when openssl is missing', () => {
    const { code, stdout } = run(binDir('no-openssl', []), 'integrity "$TGZ"')
    expect(code).not.toBe(0)
    expect(stdout).toBe('')
  })

  test('fails when the digest step fails, even if base64 still succeeds', () => {
    const path = binDir('digest-fails', [], {
      openssl: 'if [ "$1" = dgst ]; then exit 1; fi; printf ""',
    })
    const { code, stdout } = run(path, 'integrity "$TGZ"')
    expect(code).not.toBe(0)
    expect(stdout).toBe('')
  })

  test('fails when openssl prints something that is not a SHA-512 digest', () => {
    const path = binDir('garbage', [], { openssl: 'cat >/dev/null; printf "not-a-digest"' })
    const { code, stdout } = run(path, 'integrity "$TGZ"')
    expect(code).not.toBe(0)
    expect(stdout).toBe('')
  })
})

describe('approve-packages.sh staged_matches_local', () => {
  // `npm stage download` writes the staged tarball into the current directory.
  const npm = 'if [ "$1 $2" = "stage download" ]; then printf "staged" > staged.tgz; fi'

  test('refuses to report a match when neither tarball can be hashed', () => {
    // Before the fix, both hashes came out as a bare "sha512-", compared equal,
    // and the unverified stage was approved.
    const path = binDir('unhashable', ['mkdir', 'find'], { npm })
    const { code, stdout } = run(path, 'staged_matches_local "@temps-sdk/cli@0.1.38" stage-1 "$TGZ"')
    expect(code).not.toBe(0)
    expect(stdout).toContain('could not hash')
    expect(stdout).not.toContain('matches')
  })

  test('reports a mismatch between different tarballs', () => {
    if (!Bun.which('openssl')) {
      console.log('openssl not available, skipping')
      return
    }
    const path = binDir('mismatch', ['mkdir', 'find', 'openssl'], { npm })
    const { code, stdout } = run(path, 'staged_matches_local "@temps-sdk/cli@0.1.38" stage-2 "$TGZ"')
    expect(code).not.toBe(0)
    expect(stdout).toContain('does NOT match')
  })
})

describe('approve-packages.sh entry point', () => {
  test('still prints usage when run without a tarball directory', () => {
    const bash = Bun.which('bash')
    if (!bash) throw new Error('bash is required to run these tests')
    const result = Bun.spawnSync([bash, SCRIPT])
    expect(result.exitCode).toBe(2)
    expect(result.stderr.toString()).toContain('usage:')
  })
})
