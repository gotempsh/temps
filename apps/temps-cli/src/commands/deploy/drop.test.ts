import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { slugName, zipDirectory } from './drop'

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
