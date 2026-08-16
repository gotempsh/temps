import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { detectLocalPackageManager, formatFileSize } from './deploy-local-image.js'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true }))
  )
})

async function fixtureDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-local-image-test-'))
  temporaryDirectories.push(directory)
  return directory
}

describe('formatFileSize', () => {
  test('renders bytes under 1 KB as whole bytes', () => {
    expect(formatFileSize(512)).toBe('512 B')
  })

  test('renders KB and MB with one decimal', () => {
    expect(formatFileSize(2048)).toBe('2.0 KB')
    expect(formatFileSize(1024 * 1024 * 3)).toBe('3.0 MB')
  })

  test('renders GB with two decimals, unlike the deploy-static formatter', () => {
    // Docker images regularly exceed 1 GB, unlike static bundles — this
    // formatter has a GB tier the deploy-static one intentionally lacks.
    expect(formatFileSize(1024 * 1024 * 1024 * 2)).toBe('2.00 GB')
  })
})

describe('detectLocalPackageManager', () => {
  test('defaults to npm when no lockfile is present', async () => {
    const dir = await fixtureDirectory()
    expect(detectLocalPackageManager(dir)).toBe('npm')
  })

  test('detects pnpm, yarn, and bun lockfiles', async () => {
    const pnpmDir = await fixtureDirectory()
    await writeFile(join(pnpmDir, 'pnpm-lock.yaml'), '')
    expect(detectLocalPackageManager(pnpmDir)).toBe('pnpm')

    const yarnDir = await fixtureDirectory()
    await writeFile(join(yarnDir, 'yarn.lock'), '')
    expect(detectLocalPackageManager(yarnDir)).toBe('yarn')

    const bunDir = await fixtureDirectory()
    await writeFile(join(bunDir, 'bun.lock'), '')
    expect(detectLocalPackageManager(bunDir)).toBe('bun')
  })

  test('prefers pnpm over yarn and npm when multiple lockfiles exist', async () => {
    // Priority order matters: a repo mid-migration between package managers
    // can have several lockfiles, and picking the wrong one generates a
    // Dockerfile that installs with the wrong tool.
    const dir = await fixtureDirectory()
    await writeFile(join(dir, 'package-lock.json'), '')
    await writeFile(join(dir, 'yarn.lock'), '')
    await writeFile(join(dir, 'pnpm-lock.yaml'), '')
    expect(detectLocalPackageManager(dir)).toBe('pnpm')
  })
})
