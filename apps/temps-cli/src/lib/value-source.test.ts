// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  MAX_VALUE_BYTES,
  ValueSourceError,
  addCredentialSources,
  addValueSourceOptions,
  resolveValueSource,
  trimOneTrailingNewline,
  valueSourceInput,
  type ValueSourceIO,
  type ValueSourceInput,
  type ValueSourceSpec,
} from './value-source.js'

const SECRET = 's3cr3t-test-value'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })))
})

async function scratch(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-value-source-'))
  temporaryDirectories.push(directory)
  return directory
}

/** A private (0600) file holding `contents`. */
async function secretFile(contents: string | Uint8Array, name = 'value.txt'): Promise<string> {
  const path = join(await scratch(), name)
  await writeFile(path, contents, { mode: 0o600 })
  await chmod(path, 0o600)
  return path
}

interface FakeIO extends ValueSourceIO {
  warnings: string[]
}

/** Captures warnings; stdin is a pipe yielding `chunks`, or a terminal. */
function fakeIO(options: { chunks?: Array<string | Uint8Array>; tty?: boolean; platform?: NodeJS.Platform } = {}): FakeIO {
  const warnings: string[] = []
  const chunks = options.chunks ?? []
  return {
    warnings,
    warn: (message) => warnings.push(message),
    platform: options.platform ?? 'linux',
    stdin: {
      isTTY: options.tty ?? false,
      async *[Symbol.asyncIterator]() {
        for (const chunk of chunks) yield chunk
      },
    },
  }
}

const SPEC: ValueSourceSpec = {
  subject: 'the value for DB_PASSWORD',
  flags: { inline: '<value>', file: '--value-file', stdin: '--value-stdin' },
}

const LEGACY_SPEC: ValueSourceSpec = {
  subject: 'the value for secret my_key',
  flags: { inline: '-v/--value', file: '--value-file', stdin: '--value-stdin' },
  legacyAtPath: true,
  required: true,
}

async function resolve(input: ValueSourceInput, spec: ValueSourceSpec = SPEC, io: FakeIO = fakeIO()) {
  return resolveValueSource(input, spec, io)
}

async function errorOf(promise: Promise<unknown>): Promise<Error> {
  try {
    await promise
  } catch (err) {
    return err as Error
  }
  throw new Error('expected the promise to reject')
}

describe('each source', () => {
  test('an inline value is passed through unchanged', async () => {
    expect(await resolve({ inline: SECRET })).toEqual({ value: SECRET, source: 'argument' })
  })

  test('--value-file reads the file', async () => {
    const path = await secretFile(SECRET)
    expect(await resolve({ file: path })).toEqual({ value: SECRET, source: 'file' })
  })

  test('--value-stdin reads all of stdin, across chunks', async () => {
    const io = fakeIO({ chunks: ['s3cr3t', new TextEncoder().encode('-test-'), 'value'] })
    expect(await resolve({ stdin: true }, SPEC, io)).toEqual({ value: SECRET, source: 'stdin' })
  })

  test('--value-file - reads stdin', async () => {
    const io = fakeIO({ chunks: [SECRET] })
    expect(await resolve({ file: '-' }, SPEC, io)).toEqual({ value: SECRET, source: 'stdin' })
  })

  test('no source resolves to undefined unless a value is required', async () => {
    expect(await resolve({})).toBeUndefined()
    const err = await errorOf(resolve({}, { ...SPEC, required: true }))
    expect(err).toBeInstanceOf(ValueSourceError)
    expect(err.message).toBe('Missing the value for DB_PASSWORD: pass --value-file <path>, --value-stdin, or <value>')
  })

  test('an empty file is sent as an empty value, but an empty pipe is an error', async () => {
    expect(await resolve({ file: await secretFile('') })).toEqual({ value: '', source: 'file' })
    const err = await errorOf(resolve({ stdin: true }, SPEC, fakeIO({ chunks: [] })))
    expect(err.message).toBe('--value-stdin was given but nothing was piped on stdin for the value for DB_PASSWORD')
  })
})

describe('conflicting sources', () => {
  test('each pair is rejected, naming both flags', async () => {
    const path = await secretFile(SECRET)
    const cases: Array<[ValueSourceInput, string]> = [
      [{ inline: SECRET, file: path }, '<value> and --value-file'],
      [{ inline: SECRET, stdin: true }, '<value> and --value-stdin'],
      [{ file: path, stdin: true }, '--value-file and --value-stdin'],
      [{ file: '-', stdin: true }, '--value-file - and --value-stdin'],
    ]
    for (const [input, named] of cases) {
      const err = await errorOf(resolve(input, SPEC, fakeIO({ chunks: [SECRET] })))
      expect(err).toBeInstanceOf(ValueSourceError)
      expect(err.message).toBe(`Pass the value for DB_PASSWORD from only one source, got ${named}`)
    }
  })

  test('all three at once names all three', async () => {
    const err = await errorOf(resolve({ inline: 'x', file: '/tmp/x', stdin: true }))
    expect(err.message).toContain('got <value> and --value-file and --value-stdin')
  })

  test('the conflict is reported before stdin is read', async () => {
    let read = false
    const io = fakeIO()
    io.stdin = {
      isTTY: false,
      async *[Symbol.asyncIterator]() {
        read = true
        yield SECRET
      },
    }
    await errorOf(resolve({ inline: 'x', stdin: true }, SPEC, io))
    expect(read).toBe(false)
  })

  test('an empty --value-file path is refused', async () => {
    const err = await errorOf(resolve({ file: '' }))
    expect(err.message).toBe('--value-file needs a path (or - for stdin)')
  })
})

describe('legacy @path form', () => {
  test('still reads the file, with a one-line deprecation note on stderr', async () => {
    const path = await secretFile(SECRET)
    const io = fakeIO()
    expect(await resolve({ inline: `@${path}` }, LEGACY_SPEC, io)).toEqual({ value: SECRET, source: 'legacy-file' })
    expect(io.warnings).toHaveLength(1)
    expect(io.warnings[0]).toContain('deprecated')
    expect(io.warnings[0]).toContain('--value-file <path>')
    expect(io.warnings[0]).not.toContain('\n')
  })

  test('--literal sends a value starting with @ as is, without a note', async () => {
    const io = fakeIO()
    expect(await resolve({ inline: `@${SECRET}`, literal: true }, LEGACY_SPEC, io)).toEqual({
      value: `@${SECRET}`,
      source: 'argument',
    })
    expect(io.warnings).toEqual([])
  })

  test('a command without the legacy form never treats @ as a path', async () => {
    const io = fakeIO()
    expect(await resolve({ inline: `@${SECRET}` }, SPEC, io)).toEqual({ value: `@${SECRET}`, source: 'argument' })
    expect(io.warnings).toEqual([])
  })

  test('an unreadable @path says how to send a literal @ value, without echoing it', async () => {
    const io = fakeIO()
    const err = await errorOf(resolve({ inline: `@${SECRET}` }, LEGACY_SPEC, io))
    expect(err).toBeInstanceOf(ValueSourceError)
    expect(err.message).toContain('ENOENT')
    expect(err.message).toContain('--literal')
    expect(err.message).not.toContain(SECRET)
    expect(io.warnings.join('\n')).not.toContain(SECRET)
  })
})

describe('trailing newlines', () => {
  test('bytes are kept exactly by default, so PEM content is not altered', async () => {
    const pem = '-----BEGIN PRIVATE KEY-----\r\nMIIB\r\n-----END PRIVATE KEY-----\r\n\r\n'
    expect((await resolve({ file: await secretFile(pem) }))!.value).toBe(pem)
    const bom = '﻿value\n'
    expect((await resolve({ file: await secretFile(bom) }))!.value).toBe(bom)
  })

  test('--trim-trailing-newline strips exactly one \\n or \\r\\n', async () => {
    expect((await resolve({ file: await secretFile(`${SECRET}\n`), trimTrailingNewline: true }))!.value).toBe(SECRET)
    expect((await resolve({ file: await secretFile(`${SECRET}\r\n`), trimTrailingNewline: true }))!.value).toBe(SECRET)
    expect((await resolve({ file: await secretFile(`${SECRET}\n\n`), trimTrailingNewline: true }))!.value).toBe(`${SECRET}\n`)
    expect((await resolve({ file: await secretFile(SECRET), trimTrailingNewline: true }))!.value).toBe(SECRET)
    const io = fakeIO({ chunks: [`${SECRET}\n`] })
    expect((await resolve({ stdin: true, trimTrailingNewline: true }, SPEC, io))!.value).toBe(SECRET)
    expect(trimOneTrailingNewline('a\r')).toBe('a\r')
  })

  test('a file value ending in a newline gets a one-line hint when the spec asks for it', async () => {
    const path = await secretFile(`${SECRET}\n`)
    const io = fakeIO()
    expect((await resolve({ file: path }, { ...SPEC, newlineHint: true }, io))!.value).toBe(`${SECRET}\n`)
    expect(io.warnings).toEqual([
      'hint: the value for DB_PASSWORD read from the file ends with a newline, which is kept; pass --trim-trailing-newline to drop it',
    ])
  })

  test('no hint without newlineHint, when trimming, or for an inline value', async () => {
    const path = await secretFile(`${SECRET}\n`)
    const quiet = fakeIO()
    await resolve({ file: path }, SPEC, quiet)
    await resolve({ file: path, trimTrailingNewline: true }, { ...SPEC, newlineHint: true }, quiet)
    await resolve({ inline: `${SECRET}\n` }, { ...SPEC, newlineHint: true }, quiet)
    expect(quiet.warnings).toEqual([])
  })
})

describe('stdin', () => {
  test('refuses an interactive terminal instead of waiting on it', async () => {
    const err = await errorOf(resolve({ stdin: true }, SPEC, fakeIO({ tty: true })))
    expect(err).toBeInstanceOf(ValueSourceError)
    expect(err.message).toContain('stdin is a terminal with nothing piped in')
    expect(err.message).toContain('--value-file <path>')
  })

  test('--value-file - on a terminal names that flag', async () => {
    const err = await errorOf(resolve({ file: '-' }, SPEC, fakeIO({ tty: true })))
    expect(err.message.startsWith('--value-file - reads')).toBe(true)
  })

  test('reads a real pipe in a child process, byte-for-byte', async () => {
    const directory = await scratch()
    const script = join(directory, 'read-stdin.ts')
    const modulePath = join(import.meta.dir, 'value-source.ts')
    await writeFile(
      script,
      `import { resolveValueSource } from ${JSON.stringify(modulePath)}\n` +
        `const r = await resolveValueSource({ stdin: true }, { subject: 'the value', flags: { inline: '<value>', file: '--value-file', stdin: '--value-stdin' } })\n` +
        `process.stdout.write(JSON.stringify(r))\n`,
    )
    const child = Bun.spawn([process.execPath, script], { stdin: 'pipe', stdout: 'pipe', stderr: 'pipe' })
    child.stdin.write(`${SECRET}\nline two\n`)
    await child.stdin.end()
    const [stdout, exitCode] = await Promise.all([new Response(child.stdout).text(), child.exited])
    expect(exitCode).toBe(0)
    expect(JSON.parse(stdout)).toEqual({ value: `${SECRET}\nline two\n`, source: 'stdin' })
  })
})

describe('size cap', () => {
  test('matches the 1 MiB the API accepts for a secret', () => {
    expect(MAX_VALUE_BYTES).toBe(1_048_576)
  })

  test('a file at the cap is accepted; one byte over is refused before reading', async () => {
    expect((await resolve({ file: await secretFile('a'.repeat(MAX_VALUE_BYTES)) }))!.value).toHaveLength(MAX_VALUE_BYTES)
    const err = await errorOf(resolve({ file: await secretFile('a'.repeat(MAX_VALUE_BYTES + 1)) }))
    expect(err).toBeInstanceOf(ValueSourceError)
    expect(err.message).toContain('larger than the 1 MiB limit')
  })

  test('stdin over the cap is refused', async () => {
    const io = fakeIO({ chunks: ['a'.repeat(MAX_VALUE_BYTES), 'b'] })
    const err = await errorOf(resolve({ stdin: true }, SPEC, io))
    expect(err.message).toBe('the value for DB_PASSWORD from stdin is larger than the 1 MiB limit the API accepts')
  })

  test('an inline value over the cap is refused, counting UTF-8 bytes', async () => {
    const err = await errorOf(resolve({ inline: 'é'.repeat(MAX_VALUE_BYTES / 2 + 1) }))
    expect(err.message).toContain('from <value> is larger than')
  })

  test('a custom cap is honoured', async () => {
    const err = await errorOf(resolve({ file: await secretFile('12345') }, { ...SPEC, maxBytes: 4 }))
    expect(err.message).toContain('4 bytes limit')
  })
})

describe('what may be read', () => {
  test('a directory is refused', async () => {
    const directory = await scratch()
    const err = await errorOf(resolve({ file: directory }))
    expect(err).toBeInstanceOf(ValueSourceError)
    expect(err.message).toBe(`Cannot read the value for DB_PASSWORD from ${directory}: it is a directory, not a file`)
  })

  // The FIFO has no writer: a blocking open would hang here. The file is opened
  // non-blocking and its descriptor is checked, so a path swapped for a FIFO
  // after any earlier check is refused the same way.
  test('a FIFO is refused without blocking on it', async () => {
    if (process.platform === 'win32') return
    const fifo = join(await scratch(), 'fifo')
    const mkfifo = Bun.spawnSync(['mkfifo', fifo])
    if (mkfifo.exitCode !== 0) return
    const err = await errorOf(resolve({ file: fifo }))
    expect(err.message).toContain('not a regular file')
    expect(err.message).toContain('--value-stdin')
  })

  test('a missing file names the path and the error code', async () => {
    const missing = join(await scratch(), 'missing.txt')
    const err = await errorOf(resolve({ file: missing }))
    expect(err.message).toBe(`Cannot read the value for DB_PASSWORD from ${missing}: ENOENT`)
  })

  test('a symlink to a regular file is followed (e.g. a mounted secret)', async () => {
    const target = await secretFile(SECRET)
    const link = join(await scratch(), 'link')
    await Bun.spawn(['ln', '-s', target, link]).exited
    expect((await resolve({ file: link }))!.value).toBe(SECRET)
  })
})

describe('file permissions', () => {
  test('warns, without blocking, when the file is group- or world-readable', async () => {
    if (process.platform === 'win32') return
    for (const mode of [0o644, 0o640, 0o604]) {
      const path = await secretFile(SECRET)
      await chmod(path, mode)
      const io = fakeIO()
      expect((await resolve({ file: path }, SPEC, io))!.value).toBe(SECRET)
      expect(io.warnings).toEqual([
        `warning: ${path} is readable by other users (mode ${mode.toString(8)}); consider \`chmod 600\` on it`,
      ])
    }
  })

  test('is quiet for a private file, and on Windows where modes do not apply', async () => {
    const path = await secretFile(SECRET)
    const io = fakeIO()
    await resolve({ file: path }, SPEC, io)
    if (process.platform !== 'win32') await chmod(path, 0o644)
    const windows = fakeIO({ platform: 'win32' })
    await resolve({ file: path }, SPEC, windows)
    expect([...io.warnings, ...windows.warnings]).toEqual([])
  })
})

describe('encoding', () => {
  test('invalid UTF-8 is reported, explaining that the API takes text, not base64', async () => {
    const path = await secretFile(new Uint8Array([0x73, 0x65, 0xff, 0xfe, 0x00]))
    const err = await errorOf(resolve({ file: path }))
    expect(err).toBeInstanceOf(ValueSourceError)
    expect(err.message).toContain(`from ${path} is not valid UTF-8 text`)
    expect(err.message).toContain('does not decode base64')
  })

  test('invalid UTF-8 on stdin is reported too', async () => {
    const io = fakeIO({ chunks: [new Uint8Array([0xc3, 0x28])] })
    const err = await errorOf(resolve({ stdin: true }, SPEC, io))
    expect(err.message).toContain('from stdin is not valid UTF-8 text')
  })

  test('multi-byte UTF-8 split across chunks is decoded intact', async () => {
    const bytes = new TextEncoder().encode('pässwörd')
    const io = fakeIO({ chunks: [bytes.subarray(0, 2), bytes.subarray(2)] })
    expect((await resolve({ stdin: true }, SPEC, io))!.value).toBe('pässwörd')
  })
})

describe('the value never leaks', () => {
  test('no warning or error carries the sentinel secret', async () => {
    const outputs: string[] = []
    const record = async (input: ValueSourceInput, spec: ValueSourceSpec, io = fakeIO()) => {
      try {
        const resolved = await resolveValueSource(input, spec, io)
        if (resolved) expect(resolved.value).toContain(SECRET)
      } catch (err) {
        outputs.push((err as Error).message, String((err as Error).stack))
      }
      outputs.push(...io.warnings)
    }
    const world = await secretFile(`${SECRET}\n`)
    await chmod(world, 0o644)
    const directory = await scratch()

    await record({ file: world }, { ...SPEC, newlineHint: true })
    await record({ inline: SECRET, file: world }, SPEC)
    await record({ inline: SECRET, stdin: true }, SPEC)
    await record({ inline: `@${SECRET}` }, LEGACY_SPEC)
    await record({ inline: `@${world}` }, { ...LEGACY_SPEC, newlineHint: true })
    await record({ stdin: true }, SPEC, fakeIO({ chunks: [`${SECRET}\n`.repeat(70_000)] }))
    await record({ stdin: true }, { ...SPEC, newlineHint: true }, fakeIO({ chunks: [`${SECRET}\n`] }))
    await record({ inline: SECRET.repeat(70_000) }, SPEC)
    await record({ file: await secretFile(new Uint8Array([...new TextEncoder().encode(SECRET), 0xff])) }, SPEC)
    await record({ file: directory }, SPEC)

    expect(outputs.length).toBeGreaterThan(5)
    for (const output of outputs) expect(output).not.toContain(SECRET)
  })
})

describe('commander wiring', () => {
  test('addValueSourceOptions registers the flags and valueSourceInput reads them back', () => {
    const cmd = new Command('set').argument('[value]')
    addValueSourceOptions(cmd, { what: 'the value', legacyAtPath: true })
    cmd.parseOptions(['--value-file', './x', '--value-stdin', '--literal', '--trim-trailing-newline'])
    expect(valueSourceInput('inline', cmd.opts())).toEqual({
      inline: 'inline',
      file: './x',
      stdin: true,
      literal: true,
      trimTrailingNewline: true,
    })
    const plain = new Command('set')
    addValueSourceOptions(plain, { what: 'the value' })
    expect(plain.options.map((o) => o.long)).toEqual(['--value-file', '--value-stdin', '--trim-trailing-newline'])
  })

  async function runCredentials(argv: string[], io: FakeIO): Promise<{ opts: Record<string, unknown>; error?: Error }> {
    const program = new Command().exitOverride()
    let seen: Record<string, unknown> = {}
    const cmd = program
      .command('connect')
      .option('--token <token>')
      .option('--password <password>')
      .action((options) => {
        seen = { ...options }
      })
    addCredentialSources(
      cmd,
      [
        { flag: 'token', what: 'the access token' },
        { flag: 'password', what: 'the app password' },
      ],
      () => io,
    )
    try {
      await program.parseAsync(['connect', ...argv], { from: 'user' })
      return { opts: seen }
    } catch (err) {
      return { opts: seen, error: err as Error }
    }
  }

  test('credential --<flag>-file and --<flag>-stdin land in the original option', async () => {
    const path = await secretFile(SECRET)
    const run = await runCredentials(['--token-file', path, '--password-stdin'], fakeIO({ chunks: ['app-pass'] }))
    expect(run.error).toBeUndefined()
    expect(run.opts.token).toBe(SECRET)
    expect(run.opts.password).toBe('app-pass')
  })

  test('the inline credential flag still works', async () => {
    const run = await runCredentials(['--token', SECRET], fakeIO())
    expect(run.opts.token).toBe(SECRET)
  })

  test('two credentials cannot both come from stdin', async () => {
    const run = await runCredentials(['--token-stdin', '--password-file', '-'], fakeIO({ chunks: ['x'] }))
    expect(run.error?.message).toBe('Only one value can be read from stdin per command, got --token-stdin and --password-file -')
  })

  test('an inline credential and its file is a conflict naming both, and the action never runs', async () => {
    const path = await secretFile(SECRET)
    const run = await runCredentials(['--token', SECRET, '--token-file', path], fakeIO())
    expect(run.error?.message).toBe('Pass the access token from only one source, got --token and --token-file')
    expect(run.error?.message).not.toContain(SECRET)
    expect(run.opts).toEqual({})
  })

  test('a credential file ending in a newline is kept, with a hint, unless trimmed', async () => {
    const path = await secretFile(`${SECRET}\n`)
    const io = fakeIO()
    expect((await runCredentials(['--token-file', path], io)).opts.token).toBe(`${SECRET}\n`)
    expect(io.warnings[0]).toContain('--trim-trailing-newline')
    expect((await runCredentials(['--token-file', path, '--trim-trailing-newline'], fakeIO())).opts.token).toBe(SECRET)
  })

  test('an empty credential file, or one holding only a newline when trimming, is refused', async () => {
    const empty = await runCredentials(['--token-file', await secretFile('')], fakeIO())
    expect(empty.error?.message).toBe('the access token read from the file is empty')
    expect(empty.opts).toEqual({})
    const newline = await runCredentials(['--token-file', await secretFile('\n'), '--trim-trailing-newline'], fakeIO())
    expect(newline.error?.message).toBe('the access token read from the file is empty after trimming the trailing newline')
    const stdin = await runCredentials(['--token-stdin', '--trim-trailing-newline'], fakeIO({ chunks: ['\r\n'] }))
    expect(stdin.error?.message).toBe('the access token read from stdin is empty after trimming the trailing newline')
  })

  test('directories are refused for credential files too', async () => {
    const directory = await scratch()
    await mkdir(join(directory, 'sub'))
    const run = await runCredentials(['--token-file', join(directory, 'sub')], fakeIO())
    expect(run.error?.message).toContain('it is a directory, not a file')
  })
})

describe('every credential flag in the CLI', () => {
  // Options that carry a secret on the command line. Each must offer a way to
  // keep it out of shell history: `--<flag>-file` and `--<flag>-stdin` from
  // this helper, or (for the DNS commands, which predate it) `--<flag>-stdin`.
  const CREDENTIAL_FLAGS = new Set([
    '--token', '--password', '--api-key', '--api-token', '--secret', '--secret-key',
    '--secret-access-key', '--client-secret', '--private-key', '--bot-token', '--git-password',
    '--webhook-url', '--value',
  ])

  // `--value` here is ordinary data, not a credential.
  const NOT_CREDENTIALS = new Set([
    'temps dns records set --value', // a public DNS record
    'temps settings update --value', // platform settings: URLs, booleans, numbers
    'temps kv set --value', // application data in the KV store
    'temps notification-preferences update --value', // a preference flag
  ])

  test('has a file or stdin alternative', async () => {
    const { createProgram } = await import('../cli.js')
    const missing: string[] = []
    const walk = (cmd: Command, path: string) => {
      const longs = new Set(cmd.options.map((o) => o.long))
      for (const option of cmd.options) {
        if (!option.long || !CREDENTIAL_FLAGS.has(option.long) || option.negate || !option.required) continue
        const hasAlternative = longs.has(`${option.long}-stdin`) || longs.has(`${option.long}-file`)
        if (!hasAlternative && !NOT_CREDENTIALS.has(`${path} ${option.long}`)) missing.push(`${path} ${option.long}`)
      }
      for (const sub of cmd.commands) walk(sub, `${path} ${sub.name()}`)
    }
    walk(createProgram(), 'temps')
    expect(missing).toEqual([])
  })
})
