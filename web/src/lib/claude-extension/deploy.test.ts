// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, spyOn, test } from 'bun:test'
import {
  appendFileSync,
  chmodSync,
  mkdirSync,
  mkdtempSync,
  promises as fsPromises,
  realpathSync,
  renameSync,
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
  regex: RegExp
}
interface Collected {
  files: { name: string }[]
  leftOut: string[]
  leftOutCount: number
  total: number
}
interface Deploy {
  alwaysLeftOut(name: string, isDirectory: boolean): boolean
  parseIgnore(text: string, base: string, ignoreCase?: boolean): Rule[]
  isIgnored(rules: Rule[], relative: string, isDirectory: boolean): boolean
  allowedRoot(input: string, folders: string[], host?: Host): Promise<string>
  HOSTS: { extension: Host; cli: Host }
  collectFiles(
    root: string,
    limits?: {
      files?: number
      bytes?: number
      ignoreBytes?: number
      entries?: number
    }
  ): Promise<Collected>
  deployFolder(
    config: { folders: string[]; origin: string },
    args: Record<string, unknown>,
    api: (
      method: string,
      path: string
    ) => Promise<{
      status: number
      headers: Record<string, string>
      text: string
    }>
  ): Promise<string>
  packZip(
    collected: Collected,
    limits?: { files?: number; bytes?: number }
  ): Promise<{
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
    // Credential stores kept in a project for a mounted configuration.
    // Whatever the case: on macOS and Windows `.ENV` is the file `.env`.
    for (const name of ['.ENV', '.Env.Local', 'Credentials.JSON', 'ID_RSA']) {
      expect(deploy.alwaysLeftOut(name, false)).toBe(true)
    }
    for (const name of ['.GIT', 'Node_Modules']) {
      expect(deploy.alwaysLeftOut(name, true)).toBe(true)
    }
    // A linked worktree's or submodule's `.git` is a file naming its repository.
    expect(deploy.alwaysLeftOut('.git', false)).toBe(true)
    for (const store of [
      '.aws',
      '.azure',
      '.docker',
      '.gnupg',
      '.kube',
      '.ssh',
    ]) {
      expect(deploy.alwaysLeftOut(store, true)).toBe(true)
    }
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

  test('bracket expressions read POSIX classes, negation and escapes as git does', () => {
    const classes = deploy.parseIgnore(
      [
        'credential-[[:digit:]].json',
        'key[[:upper:][:digit:]]',
        'note[!a-c].md',
        'raw[]x]',
        'odd[[:nosuch:]]',
      ].join('\n'),
      '',
      // Case matters to [:upper:], as where file names have case.
      false
    )
    const hit = (p: string) => deploy.isIgnored(classes, p, false)
    expect(hit('credential-1.json')).toBe(true)
    expect(hit('config/credential-7.json')).toBe(true)
    expect(hit('credential-x.json')).toBe(false)
    expect(hit('credential-1]json')).toBe(false)
    expect(hit('keyA')).toBe(true)
    expect(hit('key4')).toBe(true)
    expect(hit('keya')).toBe(false)
    expect(hit('noted.md')).toBe(true)
    expect(hit('notea.md')).toBe(false)
    expect(hit('raw]')).toBe(true)
    expect(hit('rawx')).toBe(true)
    // An unknown class matches nothing, as in git.
    expect(hit('oddn')).toBe(false)
  })

  test('a ? is one byte of the name, as git reads it, so a wildcard covers a letter of two bytes', () => {
    for (const ignoreCase of [false, true]) {
      const rules = deploy.parseIgnore(
        ['secret-??.txt', 'key-?.pem', 'carpeta-ñ/'].join('\n'),
        '',
        ignoreCase
      )
      const hit = (p: string, dir = false) => deploy.isIgnored(rules, p, dir)
      // é and ñ are two bytes in UTF-8, € three.
      expect(hit('secret-é.txt')).toBe(true)
      expect(hit('nested/secret-é.txt')).toBe(true)
      expect(hit('secret-ab.txt')).toBe(true)
      expect(hit('secret-a.txt')).toBe(false)
      expect(hit('secret-€.txt')).toBe(false)
      expect(hit('key-é.pem')).toBe(false)
      expect(hit('carpeta-ñ', true)).toBe(true)
    }
  })

  test('without case, only ASCII letters are folded, as git folds them', () => {
    const rules = deploy.parseIgnore(
      ['Secret-É.txt', 'key[[:upper:]]'].join('\n'),
      '',
      true
    )
    const hit = (p: string) => deploy.isIgnored(rules, p, false)
    expect(hit('SECRET-É.TXT')).toBe(true)
    expect(hit('secret-É.txt')).toBe(true)
    // É and é differ: git folds no letter outside ASCII.
    expect(hit('secret-é.txt')).toBe(false)
    expect(hit('keya')).toBe(true)
    expect(hit('keyA')).toBe(true)
  })

  test('without case, a range keeps the endpoints it was written with', () => {
    for (const ignoreCase of [false, true]) {
      const rules = deploy.parseIgnore(
        'private[A-z].txt\n[A-z].fixture',
        '',
        ignoreCase
      )
      const hit = (p: string) => deploy.isIgnored(rules, p, false)
      // _ is between Z and a: inside [A-z], as git reads it.
      expect(hit('private_.txt')).toBe(true)
      expect(hit('_.fixture')).toBe(true)
      expect(hit('privateq.txt')).toBe(true)
      expect(hit('private-.txt')).toBe(false)
      expect(hit('PRIVATE_.TXT')).toBe(ignoreCase)
    }
    const upper = deploy.parseIgnore('key[B-D]', '', true)
    expect(deploy.isIgnored(upper, 'keyc', false)).toBe(true)
    expect(deploy.isIgnored(upper, 'keyC', false)).toBe(true)
    expect(deploy.isIgnored(upper, 'keye', false)).toBe(false)
  })

  test('a reversed range, an upper-case member or an unclosed bracket match as in git', () => {
    // Each checked against git check-ignore, with core.ignoreCase off and on.
    const cases: [string, string, boolean, boolean][] = [
      // pattern, name, ignored with case, ignored without
      ['[a-Z].fixture', 'a.fixture', true, true],
      ['[a-Z].fixture', 'A.fixture', false, true],
      ['[a-Z].fixture', 'b.fixture', false, false],
      ['[Z-a].fixture', '_.fixture', true, true],
      ['[Z-a].fixture', 'z.fixture', false, true],
      ['[Z-a].fixture', 'Z.fixture', true, true],
      ['[b-a].fixture', 'b.fixture', true, true],
      ['[b-a].fixture', 'a.fixture', false, false],
      // Without case git folds the name, not the bracket: [A] then matches
      // neither A nor a, and [!A] both.
      ['[A].fixture', 'A.fixture', true, false],
      ['[!A].fixture', 'A.fixture', false, true],
      ['[[:lower:]].fixture', 'A.fixture', false, true],
      // A bracket never closed: git matches nothing with the pattern.
      ['secret[b', 'secret[b', false, false],
    ]
    for (const [pattern, name, withCase, withoutCase] of cases) {
      for (const ignoreCase of [false, true]) {
        const rules = deploy.parseIgnore(pattern, '', ignoreCase)
        expect([
          pattern,
          name,
          ignoreCase,
          deploy.isIgnored(rules, name, false),
        ]).toEqual([
          pattern,
          name,
          ignoreCase,
          ignoreCase ? withoutCase : withCase,
        ])
      }
    }
  })

  test('a negated bracket stays a short class, however many there are', () => {
    const pattern = '[!a]'.repeat(128)
    const [rule] = deploy.parseIgnore(pattern, '', true)
    expect(rule.regex.source.length).toBeLessThan(128 * 40)
    expect(deploy.isIgnored([rule], 'b'.repeat(128), false)).toBe(true)
    expect(deploy.isIgnored([rule], `A${'b'.repeat(127)}`, false)).toBe(false)
  })

  test('a bracket never matches a slash, and a hyphen first after ! is literal', () => {
    const hyphen = deploy.parseIgnore('credential[!-a].json', '')
    // [!-a] is "not - and not a": 1 is excluded, as git excludes it.
    expect(deploy.isIgnored(hyphen, 'credential1.json', false)).toBe(true)
    expect(deploy.isIgnored(hyphen, 'credential-.json', false)).toBe(false)
    expect(deploy.isIgnored(hyphen, 'credentiala.json', false)).toBe(false)

    const exception = deploy.parseIgnore(
      ['*.json', '!config[[:punct:]]local.json', 'a[/]b', 'x[!y]z'].join('\n'),
      ''
    )
    const hit = (p: string) => deploy.isIgnored(exception, p, false)
    // The exception re-includes config-local.json, never config/local.json.
    expect(hit('config-local.json')).toBe(false)
    expect(hit('config/local.json')).toBe(true)
    expect(hit('a/b')).toBe(false)
    expect(hit('x/z')).toBe(false)
    expect(hit('xqz')).toBe(true)
  })

  test('a run of more than two stars reads as ** across folders, as in git', () => {
    // Each checked against git check-ignore.
    const rules = deploy.parseIgnore(
      ['private/***/credentials.txt', 'a***b', 'cache/****'].join('\n'),
      ''
    )
    const hit = (p: string) => deploy.isIgnored(rules, p, false)
    expect(hit('private/a/b/credentials.txt')).toBe(true)
    expect(hit('private/credentials.txt')).toBe(true)
    expect(hit('aqb')).toBe(true)
    expect(hit('aq/b')).toBe(false)
    expect(hit('cache/x/y.bin')).toBe(true)
  })

  test('a name with a newline in it is matched by ** as git matches it', () => {
    // Each checked against git check-ignore.
    const rules = deploy.parseIgnore(
      ['cache/***', 'logs/**', '**/secret.txt'].join('\n'),
      ''
    )
    const hit = (p: string) => deploy.isIgnored(rules, p, false)
    expect(hit('cache/a\nb')).toBe(true)
    expect(hit('logs/x\ny/z')).toBe(true)
    expect(hit('q\nr/secret.txt')).toBe(true)
    expect(hit('other\nname')).toBe(false)
  })

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

  test('a byte order mark does not hide the first rule', () => {
    const rules = deploy.parseIgnore(
      '\uFEFFscratch-private-data\n*.log\n',
      '',
      false
    )
    expect(deploy.isIgnored(rules, 'scratch-private-data', false)).toBe(true)
    expect(deploy.isIgnored(rules, 'debug.log', false)).toBe(true)
  })

  test('where file names have no case, a pattern matches whatever the case', () => {
    const blind = deploy.parseIgnore(
      'secrets.json\nbuild/\n!Keep.JSON\n',
      '',
      true
    )
    expect(deploy.isIgnored(blind, 'Secrets.JSON', false)).toBe(true)
    expect(deploy.isIgnored(blind, 'BUILD', true)).toBe(true)
    expect(deploy.isIgnored(blind, 'keep.json', false)).toBe(false)
    const exact = deploy.parseIgnore('secrets.json\n', '', false)
    expect(deploy.isIgnored(exact, 'Secrets.JSON', false)).toBe(false)
    expect(deploy.isIgnored(exact, 'secrets.json', false)).toBe(true)
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
    // A folder packing leaves out is no project root, nor anything in it.
    mkdirSync(join(allowed, 'app/.git/hooks'), { recursive: true })
    mkdirSync(join(allowed, 'app/node_modules/pkg'), { recursive: true })
    expect(await refused(join(allowed, 'app/.git'))).toContain(
      'inside .git, which is never deployed'
    )
    expect(await refused(join(allowed, 'app/.git/hooks'))).toContain(
      'inside .git'
    )
    expect(await refused(join(allowed, 'app/node_modules/pkg'))).toContain(
      'inside node_modules'
    )
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

describe('a folder with many entries left out', () => {
  const root = join(SCRATCH, 'many-left-out')
  const built: Record<string, string> = { 'package.json': '{}' }
  for (let i = 0; i < 50; i++) built[`out-${i}.log`] = 'built'
  built['.gitignore'] = '*.log\n'
  tree(root, built)

  test('keeps only the names it shows, and counts the rest', async () => {
    const collected = await deploy.collectFiles(root)
    expect(collected.files.map((f) => f.name)).toEqual([
      '.gitignore',
      'package.json',
    ])
    expect(collected.leftOut).toHaveLength(12)
    expect(collected.leftOutCount).toBe(50)
  })

  test('a folder past the entry limit is refused, its ignored entries counted', async () => {
    await expect(deploy.collectFiles(root, { entries: 20 })).rejects.toThrow(
      'more than 20 files and folders, counting those left out'
    )
  })
})

describe('a folder that changes while it is packed', () => {
  test('a folder swapped for a symlink after listing does not lead the read outside', async () => {
    const root = join(SCRATCH, 'swapped')
    tree(root, { 'package.json': '{}', 'config/app.json': '{"mine":true}' })
    tree(join(SCRATCH, 'outside-swap'), { 'app.json': 'NOT YOURS' })
    const collected = await deploy.collectFiles(root)
    expect(collected.files.map((f) => f.name).sort()).toEqual([
      'config/app.json',
      'package.json',
    ])
    // Between listing and packing, config/ becomes a symlink to a folder
    // outside with a file of the same name.
    renameSync(join(root, 'config'), join(root, 'config-old'))
    symlinkSync(join(SCRATCH, 'outside-swap'), join(root, 'config'))
    await expect(deploy.packZip(collected)).rejects.toThrow(
      'config/app.json changed while the folder was packed'
    )
  })

  test('a folder reached through a symlink above it while listing is not read', async () => {
    const root = join(SCRATCH, 'listed-through-link')
    tree(root, { 'package.json': '{}' })
    tree(join(SCRATCH, 'outside-list'), { 'sub/secret.txt': 'NOT YOURS' })
    symlinkSync(join(SCRATCH, 'outside-list'), join(root, 'linked'))
    // What the walk sees when a folder above the one it lists was swapped for
    // a symlink: a directory whose real path is not where it was listed.
    await expect(
      deploy.collectFiles(join(root, 'linked', 'sub'))
    ).rejects.toThrow('changed while it was read')
  })

  test("a folder swapped for a symlink between its listing and a file's lstat is refused", async () => {
    const root = join(SCRATCH, 'swapped-mid-walk')
    tree(root, { 'package.json': '{}', 'config/app.json': '{"mine":true}' })
    tree(join(SCRATCH, 'outside-mid-walk'), { 'app.json': 'NOT YOURS' })
    const target = join(root, 'config', 'app.json')
    const lstat = fsPromises.lstat
    let swapped = false
    // config/ is listed and checked, then swapped for a symlink to a folder
    // outside just before its file is looked at, so that look describes the
    // outside file.
    const spy = spyOn(fsPromises, 'lstat').mockImplementation((async (
      file: string,
      ...rest: unknown[]
    ) => {
      if (file === target && !swapped) {
        swapped = true
        renameSync(join(root, 'config'), join(root, 'config-old'))
        symlinkSync(join(SCRATCH, 'outside-mid-walk'), join(root, 'config'))
      }
      return (lstat as (...args: unknown[]) => unknown)(file, ...rest)
    }) as never)
    try {
      await expect(deploy.collectFiles(root)).rejects.toThrow(
        'config/app.json changed while the folder was read'
      )
      expect(swapped).toBe(true)
    } finally {
      spy.mockRestore()
    }
  })

  test('a .gitignore that appears after its lookup refuses the listing, never uploads what it lists', async () => {
    const root = join(SCRATCH, 'ignore-appears')
    tree(root, { 'package.json': '{}', 'sub/app.js': 'ok' })
    const ignore = join(root, 'sub', '.gitignore')
    const lstat = fsPromises.lstat
    let created = false
    // The lookup finds nothing; a checkout then writes the .gitignore and
    // the private file it lists before the directory is listed.
    const spy = spyOn(fsPromises, 'lstat').mockImplementation((async (
      file: string,
      ...rest: unknown[]
    ) => {
      const answer = (lstat as (...args: unknown[]) => Promise<unknown>)(
        file,
        ...rest
      )
      if (file === ignore && !created) {
        created = true
        await answer.catch(() => {})
        writeFileSync(ignore, 'private.csv\n')
        writeFileSync(join(root, 'sub', 'private.csv'), 'secret rows')
      }
      return answer
    }) as never)
    try {
      await expect(deploy.collectFiles(root)).rejects.toThrow(
        'changed while it was read'
      )
      expect(created).toBe(true)
    } finally {
      spy.mockRestore()
    }
  })

  test('a .gitignore that cannot be looked up refuses the folder, never reads as no rules', async () => {
    const root = join(SCRATCH, 'ignore-unreadable')
    tree(root, {
      'package.json': '{}',
      '.gitignore': 'private.csv\n',
      'private.csv': 'secret rows',
    })
    const ignore = join(root, '.gitignore')
    const lstat = fsPromises.lstat
    const spy = spyOn(fsPromises, 'lstat').mockImplementation((async (
      file: string,
      ...rest: unknown[]
    ) => {
      if (file === ignore) {
        throw Object.assign(new Error('I/O error'), { code: 'EIO' })
      }
      return (lstat as (...args: unknown[]) => unknown)(file, ...rest)
    }) as never)
    try {
      await expect(deploy.collectFiles(root)).rejects.toThrow(
        'could not be read (EIO)'
      )
    } finally {
      spy.mockRestore()
    }
  })

  test('a file that grew after listing is not read past the limit', async () => {
    const root = join(SCRATCH, 'growing')
    tree(root, { 'big.txt': 'x'.repeat(100), 'small.txt': 'ok' })
    const collected = await deploy.collectFiles(root)
    appendFileSync(join(root, 'big.txt'), 'y'.repeat(1000))
    await expect(deploy.packZip(collected, { bytes: 500 })).rejects.toThrow(
      'grew past'
    )
    // Growth within the limit is packed whole, as the file is now.
    const again = await deploy.collectFiles(root)
    appendFileSync(join(root, 'small.txt'), '!')
    const files = readDeflatedZip((await deploy.packZip(again)).zip)
    expect(files.get('small.txt')?.text).toBe('ok!')
  })

  test('a .gitignore past its limit is refused before it is read', async () => {
    const root = join(SCRATCH, 'huge-ignore')
    tree(root, { 'package.json': '{}', '.gitignore': `${'#'.repeat(5000)}\n` })
    await expect(
      deploy.collectFiles(root, { ignoreBytes: 1024 })
    ).rejects.toThrow('a .gitignore may be here')
  })
})

describe('registry settings whatever their case', () => {
  const root = join(SCRATCH, 'npmrc-case')
  tree(root, {
    'package.json': '{}',
    '.NPMRC': '//registry.npmjs.org/:_authToken=npm_secret\n',
    '.Yarnrc.yml': 'npmAuthToken: "yarn_secret"\n',
  })

  test('a credential in .NPMRC is left out like one in .npmrc', async () => {
    const { zip, packed } = await deploy.packZip(
      await deploy.collectFiles(root)
    )
    expect(packed).toEqual(['package.json'])
    const all = Buffer.from(zip).toString('latin1')
    expect(all).not.toContain('npm_secret')
    expect(all).not.toContain('yarn_secret')
  })
})

describe('a linked worktree', () => {
  const worktree = join(SCRATCH, 'worktree')
  tree(worktree, {
    '.git': 'gitdir: /somewhere/else/.git/worktrees/feature\n',
    'package.json': '{}',
  })

  test('its .git file, naming the repository, is never uploaded', async () => {
    const collected = await deploy.collectFiles(worktree)
    expect(collected.files.map((f) => f.name)).toEqual(['package.json'])
    expect(collected.leftOut).toEqual(['.git'])
  })
})

describe('starting the deployment', () => {
  const root = join(SCRATCH, 'deployable')
  tree(root, { 'package.json': '{}' })
  const json = { 'content-type': 'application/json' }
  /** Temps with the MCP server on (or off), answering the deploy with `text`. */
  const temps = (text: string, mcpOn = true) => {
    const calls: string[] = []
    const api = async (method: string, path: string) => {
      calls.push(`${method} ${path}`)
      if (path === '/mcp/tools') {
        return mcpOn
          ? { status: 200, headers: json, text: '{"groups":[]}' }
          : {
              status: 404,
              headers: { 'content-type': 'application/problem+json' },
              text: '{"title":"MCP Server Disabled"}',
            }
      }
      return { status: 202, headers: json, text }
    }
    return { api, calls }
  }
  const config = { folders: [root], origin: 'https://temps.example' }
  const args = { path: root, project_id: 4, environment_id: 7 }
  test('one call deploys the folder and names the deployment', async () => {
    const { api, calls } = temps('{"id":42}')
    const text = await deploy.deployFolder(config, args, api)
    expect(text).toContain('Deployment 42 of project 4')
    expect(calls).toEqual([
      'GET /mcp/tools',
      'POST /api/projects/4/environments/7/deploy/source',
    ])
  })

  test('with the MCP server turned off nothing is deployed', async () => {
    const off = temps('{"id":42}', false)
    await expect(deploy.deployFolder(config, args, off.api)).rejects.toThrow(
      'MCP server is turned off'
    )
    expect(off.calls).toEqual(['GET /mcp/tools'])
  })

  test('an answer naming no deployment is a failure, not a started build', async () => {
    for (const text of ['{}', '{"id":null}', '{"state":"pending"}']) {
      const { api } = temps(text)
      await expect(deploy.deployFolder(config, args, api)).rejects.toThrow(
        'did not say which deployment it started'
      )
    }
  })
})

describe('a folder inside a repository', () => {
  const repo = join(SCRATCH, 'monorepo')
  tree(repo, {
    '.git/HEAD': 'ref: refs/heads/main\n',
    '.gitignore': 'apps/site/private-config.json\n*.secret\napps/hidden/\n',
    'apps/.gitignore': 'site/generated/\n',
    'apps/site/package.json': '{}',
    'apps/site/private-config.json': '{"token":"PRIVATE"}',
    'apps/site/notes.secret': 'PRIVATE',
    'apps/site/generated/out.js': 'built',
    'apps/site/src/index.js': 'ok',
    'apps/hidden/package.json': '{}',
    'apps/nested/.git/HEAD': 'ref: refs/heads/main\n',
    'apps/nested/notes.secret': 'its own repository',
  })

  test('the .gitignore files above it, up to the repository top, still apply', async () => {
    const collected = await deploy.collectFiles(join(repo, 'apps', 'site'))
    expect(collected.files.map((f) => f.name).sort()).toEqual([
      'package.json',
      'src/index.js',
    ])
    expect(collected.leftOut.sort()).toEqual([
      'generated/',
      'notes.secret',
      'private-config.json',
    ])
    const { zip } = await deploy.packZip(collected)
    expect(Buffer.from(zip).toString('latin1')).not.toContain('PRIVATE')
  })

  test('a folder the repository leaves out uploads nothing', async () => {
    const collected = await deploy.collectFiles(join(repo, 'apps', 'hidden'))
    expect(collected.files).toEqual([])
    expect(collected.leftOut[0]).toContain('apps/hidden/')
  })

  test('a folder that is its own repository follows only its own rules', async () => {
    const collected = await deploy.collectFiles(join(repo, 'apps', 'nested'))
    expect(collected.files.map((f) => f.name)).toEqual(['notes.secret'])
  })
})
