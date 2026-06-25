import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { detectStaticDir } from './detect-project.js'

describe('detectStaticDir', () => {
  let dir: string

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'temps-static-'))
  })

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true })
  })

  test('returns null when no candidate folder or root index.html exists', () => {
    expect(detectStaticDir(dir)).toBeNull()
  })

  test('detects a build-output folder that contains index.html', () => {
    mkdirSync(join(dir, 'dist'))
    writeFileSync(join(dir, 'dist', 'index.html'), '<html></html>')
    expect(detectStaticDir(dir)).toBe('dist')
  })

  test('prefers an index.html-bearing candidate over a bare one', () => {
    // `dist` exists but is empty; `build` has the real entrypoint.
    mkdirSync(join(dir, 'dist'))
    mkdirSync(join(dir, 'build'))
    writeFileSync(join(dir, 'build', 'index.html'), '<html></html>')
    expect(detectStaticDir(dir)).toBe('build')
  })

  test('falls back to a bare candidate folder when none has index.html', () => {
    mkdirSync(join(dir, 'out'))
    writeFileSync(join(dir, 'out', 'app.js'), 'console.log(1)')
    expect(detectStaticDir(dir)).toBe('out')
  })

  test('respects candidate ordering (dist before build)', () => {
    mkdirSync(join(dir, 'build'))
    mkdirSync(join(dir, 'dist'))
    // Neither has index.html → first candidate in the ordered list wins.
    expect(detectStaticDir(dir)).toBe('dist')
  })

  test('treats a root index.html (no build step) as the current directory', () => {
    writeFileSync(join(dir, 'index.html'), '<html></html>')
    expect(detectStaticDir(dir)).toBe('.')
  })
})
