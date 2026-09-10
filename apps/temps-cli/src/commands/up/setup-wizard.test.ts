// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { detectPackageManager, matchesConnectionHost } from './setup-wizard.js'

describe('matchesConnectionHost', () => {
  test('matches a github connection to a github.com remote', () => {
    expect(matchesConnectionHost('github_app', 'github.com')).toBe(true)
  })

  test('matches a self-hosted gitlab host', () => {
    expect(matchesConnectionHost('gitlab', 'gitlab.mycompany.com')).toBe(true)
  })

  test('does not cross-match different providers', () => {
    expect(matchesConnectionHost('github_app', 'gitlab.com')).toBe(false)
  })

  test('treats a missing account type as no match rather than throwing', () => {
    expect(matchesConnectionHost(null, 'github.com')).toBe(false)
    expect(matchesConnectionHost(undefined, 'github.com')).toBe(false)
  })

  test('is case-insensitive on both sides', () => {
    expect(matchesConnectionHost('GitHub_App', 'GITHUB.COM')).toBe(true)
  })
})

describe('detectPackageManager', () => {
  const temporaryDirectories: string[] = []

  afterEach(async () => {
    await Promise.all(
      temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true }))
    )
  })

  async function fixtureDirectory(): Promise<string> {
    const directory = await mkdtemp(join(tmpdir(), 'temps-setup-wizard-test-'))
    temporaryDirectories.push(directory)
    return directory
  }

  test('detects pnpm from its lockfile', async () => {
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'pnpm-lock.yaml'), '')
    expect(detectPackageManager(dir)).toBe('pnpm')
  })

  test('detects yarn from its lockfile', async () => {
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'yarn.lock'), '')
    expect(detectPackageManager(dir)).toBe('yarn')
  })

  test('detects bun from bun.lock or bun.lockb', async () => {
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'bun.lock'), '')
    expect(detectPackageManager(dir)).toBe('bun')
  })

  test('detects npm from package-lock.json', async () => {
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'package-lock.json'), '')
    expect(detectPackageManager(dir)).toBe('npm')
  })

  test('falls back to npm when no lockfile is present', async () => {
    const dir = await fixtureDirectory()
    expect(detectPackageManager(dir)).toBe('npm')
  })

  test('prefers pnpm over other lockfiles when multiple are present', async () => {
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'pnpm-lock.yaml'), '')
    await writeFile(join(dir, 'package-lock.json'), '')
    expect(detectPackageManager(dir)).toBe('pnpm')
  })
})
