// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { Command, Option } from 'commander'
import { readFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createProgram } from '../cli.js'

/**
 * Every shell example in these pages is checked against the real command
 * tree: each subcommand must exist and be implemented, and each flag must be
 * accepted by that command or one of its parents. Add a page here once its
 * examples have been corrected, so they cannot drift again.
 */
const CHECKED_PAGES = [
  'docs/features/backups/page.mdx',
  'docs/features/cron-jobs/page.mdx',
  'docs/features/error-tracking/page.mdx',
  'docs/features/logs/page.mdx',
  'docs/features/managed-services/page.mdx',
  'docs/features/mcp/page.mdx',
  'docs/features/monitoring/page.mdx',
  'docs/howto/cli-login/page.mdx',
  'docs/reference/cli-getting-started/page.mdx',
  'docs/tutorials/deploy-laravel/page.mdx',
  'docs/tutorials/set-up-backups-and-monitoring/page.mdx',
]

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')

interface Invocation {
  page: string
  line: number
  text: string
  /** `npm` when written as `bunx/npx @temps-sdk/cli`, `either` for a bare `temps`. */
  binary: 'npm' | 'either'
  args: string[]
}

/** Split a shell line into words, honouring quotes and stopping at a comment or control operator. */
function shellWords(line: string): string[] {
  const words: string[] = []
  let current = ''
  let inWord = false
  let quote: '"' | "'" | null = null

  for (let i = 0; i < line.length; i++) {
    const ch = line[i]!
    if (quote) {
      if (ch === quote) quote = null
      else if (ch === '\\' && quote === '"' && i + 1 < line.length) current += line[++i]
      else current += ch
      continue
    }
    if (ch === '"' || ch === "'") {
      quote = ch
      inWord = true
      continue
    }
    if (ch === '\\' && i + 1 < line.length) {
      current += line[++i]
      inWord = true
      continue
    }
    if (/\s/.test(ch)) {
      if (inWord) words.push(current)
      current = ''
      inWord = false
      continue
    }
    if (!inWord && ch === '#') break
    if (!inWord && (ch === '|' || ch === ';' || ch === '&' || ch === '>' || ch === '<')) {
      // `<placeholder>` is a documentation convention, not a redirect.
      if (ch === '<' && /^<[\w.-]+>/.test(line.slice(i))) {
        const end = line.indexOf('>', i)
        current = line.slice(i, end + 1)
        i = end
        inWord = true
        continue
      }
      break
    }
    current += ch
    inWord = true
  }
  if (inWord) words.push(current)
  return words
}

/** Shell commands inside ```bash / ```sh / ```shell fences, with `\` continuations joined. */
function shellCommands(source: string): Array<{ line: number; text: string }> {
  const commands: Array<{ line: number; text: string }> = []
  const lines = source.split('\n')
  let inShell = false
  let inFence = false
  let pending: { line: number; text: string } | null = null

  lines.forEach((raw, index) => {
    const trimmed = raw.trim()
    if (trimmed.startsWith('```')) {
      if (!inFence) {
        inFence = true
        inShell = /^```(bash|sh|shell|zsh|console)\b/.test(trimmed)
      } else {
        inFence = false
        inShell = false
        pending = null
      }
      return
    }
    if (!inShell) return

    const text: string = pending ? `${pending.text} ${trimmed}` : trimmed.replace(/^\$\s+/, '')
    const line: number = pending ? pending.line : index + 1
    if (text.endsWith('\\')) {
      pending = { line, text: text.slice(0, -1).trimEnd() }
      return
    }
    pending = null
    if (text) commands.push({ line, text })
  })
  return commands
}

function findInvocations(page: string, source: string): Invocation[] {
  const invocations: Invocation[] = []
  for (const { line, text } of shellCommands(source)) {
    const words = shellWords(text)
    const runner = words[0]
    if ((runner === 'bunx' || runner === 'npx') && /^@temps-sdk\/cli(@.+)?$/.test(words[1] ?? '')) {
      invocations.push({ page, line, text, binary: 'npm', args: words.slice(2) })
    } else if (runner === 'temps') {
      invocations.push({ page, line, text, binary: 'either', args: words.slice(1) })
    }
  }
  return invocations
}

type CommandInternals = Command & {
  _hidden?: boolean
  _actionHandler?: unknown
  _allowExcessArguments?: boolean
}

function findSubcommand(command: Command, name: string): Command | undefined {
  return command.commands.find((sub) => sub.name() === name || sub.aliases().includes(name))
}

function findOption(chain: Command[], flag: string): Option | undefined {
  for (let i = chain.length - 1; i >= 0; i--) {
    const option = chain[i]!.options.find((candidate) => candidate.long === flag || candidate.short === flag)
    if (option) return option
  }
  return undefined
}

/** `--name` or `-n`, but not a negative number such as `-1`. */
function looksLikeFlag(word: string): boolean {
  return /^--?[A-Za-z]/.test(word)
}

/** Returns why `args` would be rejected by the CLI, or null when every command and flag exists. */
function rejectReason(program: Command, args: string[]): string | null {
  const chain: Command[] = [program]
  let command = program
  const operands: string[] = []

  for (let i = 0; i < args.length; i++) {
    const word = args[i]!
    if (word === '--') {
      operands.push(...args.slice(i + 1))
      break
    }
    if (word.startsWith('-') && word !== '-') {
      const inlineValue = word.includes('=')
      const flag = inlineValue ? word.slice(0, word.indexOf('=')) : word
      if (flag === '-h' || flag === '--help') continue
      const option = findOption(chain, flag)
      if (!option) {
        return `unknown option '${flag}' for '${chain.map((c) => c.name()).join(' ')}'`
      }
      const next = args[i + 1]
      if (!inlineValue && option.required) {
        // Commander would take even `--other` as the value; in a docs example
        // that is always a missing value, and it would hide an unknown flag.
        if (next === undefined || looksLikeFlag(next)) {
          return `option '${flag}' of '${chain.map((c) => c.name()).join(' ')}' needs a value`
        }
        i++
      } else if (!inlineValue && option.optional && next !== undefined && !looksLikeFlag(next)) i++
      continue
    }
    const sub = operands.length === 0 ? findSubcommand(command, word) : undefined
    if (sub) {
      if ((sub as CommandInternals)._hidden) {
        return `'${chain.map((c) => c.name()).join(' ')} ${word}' is not implemented (hidden from --help)`
      }
      command = sub
      chain.push(sub)
      continue
    }
    if (command.commands.length > 0 && !(command as CommandInternals)._actionHandler) {
      return `unknown command '${word}' under '${chain.map((c) => c.name()).join(' ')}'`
    }
    operands.push(word)
  }

  if (command !== program && command.commands.length > 0 && !(command as CommandInternals)._actionHandler) {
    return `'${chain.map((c) => c.name()).join(' ')}' needs a subcommand`
  }
  const declared = command.registeredArguments
  const variadic = declared.some((argument) => argument.variadic)
  if (!variadic && operands.length > declared.length && (command as CommandInternals)._allowExcessArguments !== true) {
    return `too many arguments for '${chain.map((c) => c.name()).join(' ')}': ${operands.join(' ')}`
  }
  return null
}

/**
 * Commands of the Rust server binary (also installed as `temps`) that docs
 * show on purpose. A bare `temps …` example the npm CLI rejects is accepted
 * only when it fully matches one of these: the subcommand must exist and
 * every flag must be declared, with a value when it takes one. `backup` is
 * also an npm CLI alias, so a name match alone would excuse any typo.
 *
 * Shapes are read from the clap source, so a renamed server command or flag
 * fails this test. Add an entry when a page starts showing another server
 * command.
 */
const SERVER_COMMANDS: Record<string, { file: string; subcommandEnum: string }> = {
  backup: { file: 'crates/temps-cli/src/commands/backup.rs', subcommandEnum: 'BackupCommands' },
}

interface ServerSubcommand {
  /** `--flag` → whether it takes a value. */
  flags: Map<string, boolean>
  positionals: number
}

function kebab(identifier: string): string {
  return identifier
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .replace(/_/g, '-')
    .toLowerCase()
}

/** Body between `header {` and the closing brace at column 0. */
function rustBlock(source: string, header: RegExp): string | undefined {
  const match = header.exec(source)
  if (!match) return undefined
  const body = source.slice(match.index + match[0].length)
  return body.slice(0, body.indexOf('\n}'))
}

/** Fields of a clap `Args` struct. Attributes may span lines. */
function serverArgs(source: string, structName: string): ServerSubcommand {
  const body = rustBlock(source, new RegExp(`struct ${structName} \\{`))
  if (body === undefined) throw new Error(`clap struct ${structName} not found`)
  const flags = new Map<string, boolean>()
  let positionals = 0
  let attribute = ''
  let collecting = false
  for (const raw of body.split('\n')) {
    const line = raw.trim()
    if (collecting || line.startsWith('#[arg(')) {
      attribute += line
      collecting = !line.endsWith(')]')
      continue
    }
    const field = /^(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*([^,]+),?$/.exec(line)
    if (!field) continue
    if (/\blong\b/.test(attribute)) {
      const custom = /\blong\s*=\s*"([^"]+)"/.exec(attribute)
      flags.set(`--${custom ? custom[1] : kebab(field[1]!)}`, field[2]!.trim() !== 'bool')
    } else if (!/\bshort\b/.test(attribute)) {
      positionals++
    }
    attribute = ''
  }
  return { flags, positionals }
}

async function serverCommandShapes(): Promise<Map<string, Map<string, ServerSubcommand>>> {
  const shapes = new Map<string, Map<string, ServerSubcommand>>()
  for (const [command, { file, subcommandEnum }] of Object.entries(SERVER_COMMANDS)) {
    const source = await readFile(join(repositoryRoot, file), 'utf8')
    const body = rustBlock(source, new RegExp(`enum ${subcommandEnum} \\{`))
    if (body === undefined) throw new Error(`clap enum ${subcommandEnum} not found in ${file}`)
    const subcommands = new Map<string, ServerSubcommand>()
    for (const match of body.matchAll(/^\s{4}([A-Z][A-Za-z]*)\((\w+)\)/gm)) {
      subcommands.set(kebab(match[1]!), serverArgs(source, match[2]!))
    }
    shapes.set(command, subcommands)
  }
  return shapes
}

/** Why the server binary would reject `args`, or null when it matches a known shape. */
function serverRejectReason(shapes: Map<string, Map<string, ServerSubcommand>>, args: string[]): string | null {
  const [command, subcommand, ...rest] = args
  const subcommands = shapes.get(command ?? '')
  if (!subcommands) return `'${command}' is not a server command checked by this test`
  const shape = subcommands.get(subcommand ?? '')
  if (!shape) return `unknown server subcommand 'temps ${command} ${subcommand ?? ''}'`
  let positionals = 0
  for (let i = 0; i < rest.length; i++) {
    const word = rest[i]!
    if (looksLikeFlag(word)) {
      const flag = word.includes('=') ? word.slice(0, word.indexOf('=')) : word
      if (flag === '-h' || flag === '--help') continue
      const takesValue = shape.flags.get(flag)
      if (takesValue === undefined) return `unknown option '${flag}' for server 'temps ${command} ${subcommand}'`
      if (takesValue && !word.includes('=')) {
        const next = rest[i + 1]
        if (next === undefined || looksLikeFlag(next)) {
          return `option '${flag}' of server 'temps ${command} ${subcommand}' needs a value`
        }
        i++
      }
      continue
    }
    positionals++
  }
  if (positionals > shape.positionals) return `too many arguments for server 'temps ${command} ${subcommand}'`
  return null
}

/** Why an example would fail, trying the server binary only for an explicit server command. */
function exampleRejectReason(
  program: Command,
  shapes: Map<string, Map<string, ServerSubcommand>>,
  invocation: Pick<Invocation, 'binary' | 'args'>,
): string | null {
  const reason = rejectReason(program, invocation.args)
  if (reason === null || invocation.binary === 'npm') return reason
  if (!shapes.has(invocation.args[0] ?? '')) return reason
  const serverReason = serverRejectReason(shapes, invocation.args)
  return serverReason === null ? null : `${reason}; as a server command: ${serverReason}`
}

describe('documented CLI examples', () => {
  test('parses shell words, comments, placeholders and pipes', () => {
    expect(shellWords(`temps errors list --project-id 42   # note the "id"`)).toEqual([
      'temps', 'errors', 'list', '--project-id', '42',
    ])
    expect(shellWords(`temps backups show --id <backup-id> | jq .`)).toEqual([
      'temps', 'backups', 'show', '--id', '<backup-id>',
    ])
    expect(shellWords(`temps login "https://a.example.com" --context 'my ctx'`)).toEqual([
      'temps', 'login', 'https://a.example.com', '--context', 'my ctx',
    ])
  })

  test('rejects the command shapes that docs used to show', () => {
    const program = createProgram()
    expect(rejectReason(program, ['backups', 'create', '--project', 'my-app'])).toContain("unknown command 'create'")
    expect(rejectReason(program, ['service', 'create', 'postgres', 'my-db'])).toContain("unknown command 'service'")
    expect(rejectReason(program, ['login', '--url', 'https://a.example.com'])).toContain("unknown option '--url'")
    expect(rejectReason(program, ['domains', 'add', '--project', 'my-app', '--domain', 'a.example.com'])).toContain(
      "unknown option '--project'",
    )
    expect(rejectReason(program, ['exec', 'my-app', '--', 'php', 'artisan', 'migrate'])).toContain('not implemented')
    expect(rejectReason(program, ['logs', '--project', 'my-app', '--follow'])).not.toBeNull()
  })

  test('requires a value for options that take one', () => {
    const program = createProgram()
    expect(rejectReason(program, ['backups', 'show', '--id'])).toContain("option '--id'")
    // The unknown flag must not be swallowed as the ID.
    expect(rejectReason(program, ['backups', 'show', '--id', '--unknown'])).toContain('needs a value')
    expect(rejectReason(program, ['backups', 'show', '--id', '12'])).toBeNull()
    expect(rejectReason(program, ['backups', 'show', '--id=12'])).toBeNull()
  })

  test('checks server binary examples against the full clap shape', async () => {
    const program = createProgram()
    const shapes = await serverCommandShapes()
    const either = (args: string[]) => exampleRejectReason(program, shapes, { binary: 'either', args })

    expect(either(['backup', 'restore', '--backup-id', 'b-1', '--dry-run'])).toBeNull()
    expect(either(['backup', 'restore-service', '--backup-id', 'b-1', '--service-name', 'db'])).toBeNull()
    expect(either(['backup', 'restore', '--backup-id'])).toContain('needs a value')
    expect(either(['backup', 'restore', '--backup-id', 'b-1', '--bogus'])).toContain("unknown option '--bogus'")
    expect(either(['backup', 'restores'])).toContain('unknown server subcommand')
    // Names both binaries share are not a free pass for the npm CLI's flags.
    expect(either(['services', 'create', '--version', '16'])).not.toBeNull()
    // `bunx @temps-sdk/cli …` never falls back to the server binary.
    expect(
      exampleRejectReason(program, shapes, { binary: 'npm', args: ['backup', 'restore', '--backup-id', 'b-1'] }),
    ).not.toBeNull()
  })

  test('accepts parent options anywhere and positional values', () => {
    const program = createProgram()
    expect(
      rejectReason(program, ['environments', 'vars', 'set', '--project', 'my-app', 'DATABASE_URL', '--environments', 'production']),
    ).toBeNull()
    expect(rejectReason(program, ['--target-context', 'prod', 'domains', 'add', '--domain', 'a.example.com'])).toBeNull()
    expect(rejectReason(program, ['services', 'restore', '--id', '1', '--backup-id', 'x', '--new-service'])).toBeNull()
    expect(rejectReason(program, ['services', 'types', 'info', 'postgres'])).toBeNull()
  })

  test('every example in the checked pages names a real command and real flags', async () => {
    const program = createProgram()
    const shapes = await serverCommandShapes()
    expect(shapes.get('backup')?.get('restore')?.flags.get('--backup-id')).toBe(true)

    const failures: string[] = []
    let checked = 0
    for (const page of CHECKED_PAGES) {
      const source = await readFile(join(repositoryRoot, page), 'utf8')
      for (const invocation of findInvocations(page, source)) {
        checked++
        const reason = exampleRejectReason(program, shapes, invocation)
        if (reason === null) continue
        failures.push(`${invocation.page}:${invocation.line}: ${reason}\n    ${invocation.text}`)
      }
    }

    expect(checked).toBeGreaterThan(50)
    expect(failures).toEqual([])
  })
})
