import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  assertUploadable,
  buildConfigChanges,
  normalizeDirectory,
  selectCandidate,
  selectEnvironment,
  slugName,
  zipDirectory,
} from './drop'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) =>
      rm(path, { recursive: true, force: true })
    )
  )
})

async function fixtureDirectory(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-drop-test-'))
  temporaryDirectories.push(directory)
  return directory
}

describe('drop archive preparation', () => {
  test('slugifies names and falls back when no usable name exists', () => {
    expect(slugName('  My Project!  ')).toBe('my-project')
    expect(slugName('🔥')).toMatch(/^drop-[a-z0-9]{8}$/)
  })

  test('rejects symbolic links instead of following them', async () => {
    const directory = await fixtureDirectory()
    await writeFile(join(directory, 'index.html'), 'ok')
    await symlink('/etc/passwd', join(directory, 'passwd'))

    await expect(zipDirectory(directory)).rejects.toThrow(
      'does not follow symbolic links'
    )
  })

  test('does not package common secrets or dependency trees', async () => {
    const directory = await fixtureDirectory()
    await mkdir(join(directory, 'node_modules/pkg'), { recursive: true })
    await mkdir(join(directory, '.git'), { recursive: true })
    await writeFile(join(directory, 'index.html'), 'ok')
    await writeFile(join(directory, '.env'), 'TOKEN=secret')
    await writeFile(join(directory, 'server.key'), 'secret')
    await writeFile(join(directory, '.git/config'), 'secret')
    await writeFile(join(directory, 'node_modules/pkg/index.js'), 'secret')

    const archive = await zipDirectory(directory)
    try {
      const archiveText = archive.data.toString('latin1')
      expect(archiveText).toContain('index.html')
      expect(archiveText).not.toContain('.env')
      expect(archiveText).not.toContain('server.key')
      expect(archiveText).not.toContain('.git/config')
      expect(archiveText).not.toContain('node_modules')
    } finally {
      await archive.cleanup()
    }
  })
})

function candidate(directory: string, preset: string) {
  return {
    directory,
    preset,
    label: preset,
    confidence: 'high',
    reason: 'detected',
    isStatic: false,
  }
}

const environments = [
  { id: 1, name: 'staging' },
  { id: 2, name: 'production' },
] as unknown as Parameters<typeof selectEnvironment>[0]

describe('drop target selection', () => {
  test('normalizes build directories to a bare relative path', () => {
    expect(normalizeDirectory(undefined)).toBe('.')
    expect(normalizeDirectory('')).toBe('.')
    expect(normalizeDirectory('./api')).toBe('api')
    expect(normalizeDirectory('.')).toBe('.')
  })

  test('picks the first candidate when creating a project', () => {
    const candidates = [candidate('web', 'nextjs'), candidate('api', 'docker-compose')]
    expect(selectCandidate(candidates, {})?.directory).toBe('web')
  })

  test('prefers the existing project build directory over detection order', () => {
    const candidates = [candidate('web', 'nextjs'), candidate('api', 'docker-compose')]
    expect(selectCandidate(candidates, {}, './api')?.directory).toBe('api')
    // No candidate matches the stored directory — fall back rather than fail.
    expect(selectCandidate(candidates, {}, 'gone')?.directory).toBe('web')
  })

  test('explicit --preset/--directory still win over the existing directory', () => {
    const candidates = [candidate('web', 'nextjs'), candidate('api', 'docker-compose')]
    expect(selectCandidate(candidates, { preset: 'nextjs' }, 'api')?.directory).toBe('web')
    expect(selectCandidate(candidates, { directory: 'web' }, 'api')?.directory).toBe('web')
    expect(selectCandidate(candidates, { preset: 'astro' }, 'api')).toBeUndefined()
  })

  test('accepts upload-backed projects and redirects the others', () => {
    expect(() => assertUploadable({ slug: 'a', source_type: 'uploaded_source' })).not.toThrow()
    expect(() => assertUploadable({ slug: 'a', source_type: 'static_files' })).not.toThrow()
    expect(() => assertUploadable({ slug: 'paseo-hub', source_type: 'git' })).toThrow(
      /deploy --project paseo-hub/
    )
    expect(() => assertUploadable({ slug: 'paseo-hub', source_type: 'docker_image' })).toThrow(
      /deploy:image --project paseo-hub/
    )
  })

  test('reports only the build settings that actually changed', () => {
    const project = { directory: './web', preset: 'nextjs' }
    expect(buildConfigChanges(project, candidate('web', 'nextjs'))).toEqual([])
    expect(buildConfigChanges(project, candidate('api', 'nextjs'))).toEqual([
      'directory web → api',
    ])
    expect(buildConfigChanges(project, candidate('web', 'astro'))).toEqual([
      'preset nextjs → astro',
    ])
    expect(buildConfigChanges({ directory: '.', preset: null }, candidate('.', 'astro'))).toEqual([
      'preset none → astro',
    ])
  })

  test('defaults to production and reports unknown environment names', () => {
    expect(selectEnvironment(environments, undefined, 'app').name).toBe('production')
    expect(selectEnvironment(environments, 'staging', 'app').id).toBe(1)
    expect(() => selectEnvironment(environments, 'preview', 'app')).toThrow(
      /no environment named "preview" \(available: staging, production\)/
    )
    expect(() => selectEnvironment([], undefined, 'app')).toThrow('has no environments')
  })
})
