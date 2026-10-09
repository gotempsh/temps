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
      if (!inlineValue && option.required) i++
      else if (!inlineValue && option.optional && next !== undefined && !next.startsWith('-')) i++
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
 * Top-level commands of the Rust server binary, also installed as `temps`,
 * read from its clap enum so a renamed server command is noticed here too.
 * Only the command name is checked for these; their flags are not.
 */
async function serverBinaryCommands(): Promise<Set<string>> {
  const source = await readFile(join(repositoryRoot, 'crates/temps-cli/src/lib.rs'), 'utf8')
  const body = source.slice(source.indexOf('pub enum Commands {'))
  const enumBody = body.slice(0, body.indexOf('\n}'))
  const names = new Set<string>()
  for (const match of enumBody.matchAll(/^\s{4}([A-Z][A-Za-z]*)\(/gm)) {
    names.add(match[1]!.replace(/([a-z])([A-Z])/g, '$1-$2').toLowerCase())
  }
  for (const match of enumBody.matchAll(/alias\s*=\s*"([^"]+)"/g)) {
    names.add(match[1]!)
  }
  return names
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
    const serverCommands = await serverBinaryCommands()
    expect(serverCommands.has('backup')).toBe(true)
    expect(serverCommands.has('serve')).toBe(true)

    const failures: string[] = []
    let checked = 0
    for (const page of CHECKED_PAGES) {
      const source = await readFile(join(repositoryRoot, page), 'utf8')
      for (const invocation of findInvocations(page, source)) {
        checked++
        const reason = rejectReason(program, invocation.args)
        if (reason === null) continue
        // A bare `temps …` line may target the server binary instead.
        if (invocation.binary === 'either' && serverCommands.has(invocation.args[0] ?? '')) continue
        failures.push(`${invocation.page}:${invocation.line}: ${reason}\n    ${invocation.text}`)
      }
    }

    expect(checked).toBeGreaterThan(50)
    expect(failures).toEqual([])
  })
})
