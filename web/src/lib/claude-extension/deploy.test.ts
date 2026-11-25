// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import { createRequire } from 'node:module'
import { homedir, tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { readDeflatedZip } from '@/test/read-zip'

interface Host {
  folders: string
}
interface Rule {
  base: string
}
interface Collected {
  files: { name: string }[]
  leftOut: string[]
  total: number
}
interface Deploy {
  alwaysLeftOut(name: string, isDirectory: boolean): boolean
  parseIgnore(text: string, base: string): Rule[]
  isIgnored(rules: Rule[], relative: string, isDirectory: boolean): boolean
  allowedRoot(input: string, folders: string[], host?: Host): Promise<string>
  HOSTS: { extension: Host; cli: Host }
  collectFiles(
    root: string,
    limits?: { files: number; bytes: number }
  ): Promise<Collected>
  packZip(collected: Collected): Promise<{
    zip: Buffer
    packed: string[]
    bytes: number
  }>
}

const deploy = createRequire(import.meta.url)(
  fileURLToPath(new URL('./deploy.cjs', import.meta.url))
) as Deploy

// realpath: on macOS the temp folder is behind a symlink (/var → /private/var).
const SCRATCH = realpathSync(mkdtempSync(join(tmpdir(), 'temps-deploy-')))
afterAll(() => rmSync(SCRATCH, { recursive: true, force: true }))

function tree(root: string, files: Record<string, string>) {
  for (const [name, text] of Object.entries(files)) {
    const path = join(root, name)
    mkdirSync(dirname(path), { recursive: true })
    writeFileSync(path, text)
  }
}

describe('what is never uploaded', () => {
  test('secrets, version control and dependencies', () => {
    for (const name of [
      '.env',
      '.env.local',
      '.env.production',
      '.envrc',
      'server.pem',
      'tls.KEY',
      'id_rsa',
      'id_ed25519.pub',
      'credentials.json',
      'terraform.tfstate.backup',
      '.DS_Store',
    ]) {
      expect(deploy.alwaysLeftOut(name, false)).toBe(true)
    }
    for (const name of ['.git', 'node_modules', '.next', '.venv']) {
      expect(deploy.alwaysLeftOut(name, true)).toBe(true)
    }
    for (const name of [
      '.env.example',
      '.env.sample',
      'package.json',
      'environment.ts',
      'keys.ts',
      '.npmrc',
    ]) {
      expect(deploy.alwaysLeftOut(name, false)).toBe(false)
    }
    expect(deploy.alwaysLeftOut('dist', true)).toBe(false)
  })
})

describe('.gitignore', () => {
  const rules = deploy.parseIgnore(
    [
      '# build output',
      'dist/',
      '*.log',
      '!keep.log',
      '/coverage',
      'docs/**/*.pdf',
      '**/tmp',
      'secret?.txt',
      '\\#notes',
      '',
    ].join('\n'),
    ''
  )
  const ignored = (path: string, isDirectory = false) =>
    deploy.isIgnored(rules, path, isDirectory)

  test('patterns match the way git matches them', () => {
    expect(ignored('dist', true)).toBe(true)
    expect(ignored('packages/web/dist', true)).toBe(true)
    // A trailing slash matches directories only.
    expect(ignored('dist', false)).toBe(false)
    expect(ignored('debug.log')).toBe(true)
    expect(ignored('logs/server.log')).toBe(true)
    // The last matching rule wins.
    expect(ignored('keep.log')).toBe(false)
    // A leading slash anchors to the folder of the .gitignore.
    expect(ignored('coverage', true)).toBe(true)
    expect(ignored('src/coverage', true)).toBe(false)
    expect(ignored('docs/a/b/guide.pdf')).toBe(true)
    expect(ignored('docs/guide.pdf')).toBe(true)
    expect(ignored('guide.pdf')).toBe(false)
    expect(ignored('a/b/tmp', true)).toBe(true)
    expect(ignored('secret1.txt')).toBe(true)
    expect(ignored('secret12.txt')).toBe(false)
    expect(ignored('#notes')).toBe(true)
    expect(ignored('src/index.ts')).toBe(false)
  })

  test('a nested .gitignore applies under its own folder only', () => {
    const nested = deploy.parseIgnore('local.txt\n/only-here', 'sub/')
    expect(deploy.isIgnored(nested, 'sub/local.txt', false)).toBe(true)
    expect(deploy.isIgnored(nested, 'sub/deep/local.txt', false)).toBe(true)
    expect(deploy.isIgnored(nested, 'local.txt', false)).toBe(false)
    expect(deploy.isIgnored(nested, 'sub/only-here', false)).toBe(true)
    expect(deploy.isIgnored(nested, 'sub/deep/only-here', false)).toBe(false)
  })
})

describe('which folders may be deployed', () => {
  const allowed = join(SCRATCH, 'allowed')
  const outside = join(SCRATCH, 'outside')
  tree(allowed, { 'app/package.json': '{}' })
  tree(outside, { 'private/notes.txt': 'secret' })
  // A link inside the allowed folder that leads out of it.
  symlinkSync(join(outside, 'private'), join(allowed, 'escape'))
  writeFileSync(join(allowed, 'file.txt'), 'x')

  test('a folder inside an allowed one, resolved', async () => {
    expect(await deploy.allowedRoot(join(allowed, 'app'), [allowed])).toBe(
      join(allowed, 'app')
    )
    expect(await deploy.allowedRoot(`${allowed}/app/../app`, [allowed])).toBe(
      join(allowed, 'app')
    )
  })

  test('anything else is refused with what to do', async () => {
    const refused = async (input: string, folders = [allowed]) => {
      let message = ''
      try {
        await deploy.allowedRoot(input, folders)
      } catch (error) {
        message = (error as Error).message
      }
      return message
    }
    expect(await refused(join(allowed, 'app'), [])).toContain(
      'Folders Claude may deploy'
    )
    expect(await refused(join(outside, 'private'))).toContain(
      'outside the folders Claude may deploy'
    )
    // Through a symlink: what counts is where it leads.
    expect(await refused(join(allowed, 'escape'))).toContain(
      'outside the folders Claude may deploy'
    )
    expect(await refused(`${allowed}/../outside/private`)).toContain(
      'outside the folders Claude may deploy'
    )
    // A sibling whose name starts with the allowed folder's name.
    mkdirSync(`${allowed}-other`, { recursive: true })
    expect(await refused(`${allowed}-other`)).toContain('outside the folders')
    expect(await refused('code/my-app')).toContain('not a full path')
    expect(await refused(join(allowed, 'missing'))).toContain('does not exist')
    expect(await refused(join(allowed, 'file.txt'))).toContain('is a file')
    expect(await refused(homedir(), [homedir()])).toContain(
      'not a project folder'
    )
    expect(await refused('/', ['/'])).toContain('not a project folder')
  })

  test('for an app the Temps CLI set up, it says how to allow a folder there', async () => {
    const messages: string[] = []
    for (const folders of [[], [allowed]]) {
      try {
        await deploy.allowedRoot(
          join(outside, 'private'),
          folders,
          deploy.HOSTS.cli
        )
      } catch (error) {
        messages.push((error as Error).message)
      }
    }
    expect(messages[0]).toContain('temps mcp add <app> --deploy-folder')
    expect(messages[1]).toContain('outside the folders this agent may deploy')
    expect(messages[1]).toContain('temps mcp add <app> --deploy-folder')
    expect(messages.join(' ')).not.toContain('Claude')
  })
})

describe('packing a folder', () => {
  const root = join(SCRATCH, 'project')
  tree(root, {
    'package.json': '{"name":"app"}',
    'src/index.js': 'console.log("hi")\n'.repeat(50),
    '.env': 'TOKEN=secret',
    '.env.example': 'TOKEN=',
    'node_modules/dep/index.js': 'module.exports = 1',
    '.git/config': '[core]',
    '.gitignore': 'dist/\n*.log\n!keep.log\n',
    'dist/out.js': 'built',
    'debug.log': 'noise',
    'keep.log': 'kept',
    'keys/id_rsa': 'PRIVATE',
    'sub/.gitignore': 'local.txt\n',
    'sub/local.txt': 'mine',
    'sub/ok.txt': 'ok',
    '.npmrc': '//registry.npmjs.org/:_authToken=npm_secret\n',
    'run.sh': '#!/bin/sh\necho run\n',
  })
  chmodSync(join(root, 'run.sh'), 0o755)
  tree(join(SCRATCH, 'elsewhere'), { 'hosts.txt': 'not yours' })
  symlinkSync(join(SCRATCH, 'elsewhere', 'hosts.txt'), join(root, 'link.txt'))
  symlinkSync(join(SCRATCH, 'elsewhere'), join(root, 'linked-dir'))

  test('only what the build needs goes in, and the rest is named', async () => {
    const collected = await deploy.collectFiles(root)
    const { zip, packed } = await deploy.packZip(collected)
    const files = readDeflatedZip(zip)
    expect([...files.keys()].sort()).toEqual([
      '.env.example',
      '.gitignore',
      'keep.log',
      'package.json',
      'run.sh',
      'src/index.js',
      'sub/.gitignore',
      'sub/ok.txt',
    ])
    expect(packed.sort()).toEqual([...files.keys()].sort())
    expect(files.get('src/index.js')?.text).toBe(
      'console.log("hi")\n'.repeat(50)
    )
    expect((files.get('run.sh')?.mode ?? 0) & 0o777).toBe(0o755)
    expect((files.get('package.json')?.mode ?? 0) & 0o777).toBe(0o644)
    expect(collected.leftOut.sort()).toEqual([
      '.env',
      '.git/',
      '.npmrc',
      'debug.log',
      'dist/',
      'keys/id_rsa',
      'link.txt',
      'linked-dir',
      'node_modules/',
      'sub/local.txt',
    ])
    const all = Buffer.from(zip).toString('latin1')
    expect(all).not.toContain('secret')
    expect(all).not.toContain('not yours')
  })

  test('the same folder makes the same archive', async () => {
    const first = await deploy.packZip(await deploy.collectFiles(root))
    const second = await deploy.packZip(await deploy.collectFiles(root))
    expect(Buffer.compare(first.zip, second.zip)).toBe(0)
  })

  test('a folder over the limits is refused before anything is read', async () => {
    await expect(
      deploy.collectFiles(root, { files: 3, bytes: 10_000_000 })
    ).rejects.toThrow('more than 3 files')
    await expect(
      deploy.collectFiles(root, { files: 100, bytes: 100 })
    ).rejects.toThrow('.gitignore')
  })
})
