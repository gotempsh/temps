// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The console's "Hand off to agent" control copies a prompt that tells an
 * agent which CLI commands to run (`web/src/lib/agent-handoff.ts`). Nothing
 * else ties that text to this CLI: rename a flag here and the console would
 * go on handing out a prompt that no longer works, and the first to find out
 * would be someone's agent.
 *
 * This test parses every command the builder can emit against the commands
 * and flags registered in `createProgram()`. It lives with the CLI because
 * it needs the CLI's definitions; the builder has no imports, so it loads
 * here as it is.
 */

import { describe, expect, test } from 'bun:test'
import type { Command, Option } from 'commander'
import { createProgram } from '../cli.js'
import {
  HANDOFF_SUBJECT_EXAMPLES,
  planAgentHandoff,
  type HandoffArg,
  type HandoffCommand,
  type HandoffSubject,
} from '../../../../web/src/lib/agent-handoff.ts'

interface ResolvedCommand {
  /** `sandbox fs read` */
  path: string
  flags: string[]
  positionals: number
}

function findSubcommand(parent: Command, name: string): Command | undefined {
  return parent.commands.find((c) => c.name() === name || c.aliases().includes(name))
}

function findOption(command: Command, flag: string): Option | undefined {
  return command.options.find((o) => o.long === flag || o.short === flag)
}

function takesValue(option: Option): boolean {
  return option.required || option.optional
}

/**
 * Resolve one emitted command the way commander would: global options, then
 * the subcommand path, then the leaf's flags and positionals. Throws naming
 * the word the CLI does not know.
 */
function resolveAgainstCli(program: Command, args: HandoffArg[]): ResolvedCommand {
  const words = args.map((arg) => (typeof arg === 'string' ? arg : `<${arg.placeholder}>`))
  const rendered = words.join(' ')
  let index = 0

  // Global options come before the subcommand.
  while (index < words.length && (words[index] as string).startsWith('--')) {
    const flag = words[index] as string
    const option = findOption(program, flag)
    if (!option) throw new Error(`\`temps ${rendered}\`: the CLI has no global option ${flag}`)
    index += takesValue(option) ? 2 : 1
  }

  let command = program
  const path: string[] = []
  while (index < words.length) {
    const next = findSubcommand(command, words[index] as string)
    if (!next) break
    command = next
    path.push(next.name())
    index += 1
  }
  if (command === program) {
    throw new Error(`\`temps ${rendered}\`: the CLI has no command "${words[index]}"`)
  }
  if (command.commands.length > 0 && command.registeredArguments.length === 0) {
    throw new Error(
      `\`temps ${rendered}\`: "${path.join(' ')}" is a command group, and "${words[index]}" is not one of its commands`,
    )
  }

  const flags: string[] = []
  let positionals = 0
  let passthrough = false
  while (index < words.length) {
    const word = words[index] as string
    if (!passthrough && word === '--') {
      passthrough = true
      index += 1
      continue
    }
    if (!passthrough && word.startsWith('-') && word.length > 1) {
      const option = findOption(command, word)
      if (!option) {
        throw new Error(`\`temps ${rendered}\`: "${path.join(' ')}" has no option ${word}`)
      }
      flags.push(option.long ?? word)
      if (takesValue(option)) {
        if (index + 1 >= words.length) {
          throw new Error(`\`temps ${rendered}\`: ${word} needs a value`)
        }
        index += 2
      } else {
        index += 1
      }
      continue
    }
    positionals += 1
    index += 1
  }

  const declared = command.registeredArguments
  const required = declared.filter((a) => a.required).length
  const variadic = declared.some((a) => a.variadic)
  if (positionals < required || (!variadic && positionals > declared.length)) {
    throw new Error(
      `\`temps ${rendered}\`: "${path.join(' ')}" takes ${declared
        .map((a) => a.name())
        .join(', ') || 'no arguments'}, got ${positionals} positional argument(s)`,
    )
  }
  for (const option of command.options) {
    if (option.mandatory && option.long && !flags.includes(option.long)) {
      throw new Error(`\`temps ${rendered}\`: "${path.join(' ')}" requires ${option.long}`)
    }
  }
  return { path: path.join(' '), flags, positionals }
}

const ORIGIN = 'https://temps.example.com'

function cliCommands(subject: HandoffSubject): HandoffCommand[] {
  const plan = planAgentHandoff({ origin: ORIGIN, cliVersion: '0.0.0', subject })
  return [
    plan.connect.check,
    plan.connect.startLogin,
    plan.connect.finishLogin,
    ...plan.read,
  ].filter((command) => command.program === 'cli')
}

const program = createProgram()
const examples = Object.entries(HANDOFF_SUBJECT_EXAMPLES) as Array<
  [HandoffSubject['kind'], HandoffSubject[]]
>

describe('every command a handoff prompt can emit exists in this CLI', () => {
  for (const [kind, subjects] of examples) {
    subjects.forEach((subject, position) => {
      test(`${kind} #${position + 1}`, () => {
        for (const command of cliCommands(subject)) {
          expect(() => resolveAgainstCli(program, command.args)).not.toThrow()
        }
      })
    })
  }

  test('has an example for every kind of subject', () => {
    for (const [kind, subjects] of examples) {
      expect(subjects.length, kind).toBeGreaterThan(0)
    }
  })
})

describe('the commands are the ones the prompt describes', () => {
  const resolved = (subject: HandoffSubject) =>
    cliCommands(subject).map((command) => resolveAgainstCli(program, command.args))

  test('connects with whoami and the two halves of the browser sign-in', () => {
    const [check, start, finish] = resolved(HANDOFF_SUBJECT_EXAMPLES.trace[0] as HandoffSubject)
    expect(check).toEqual({ path: 'whoami', flags: ['--json'], positionals: 0 })
    expect(start).toEqual({ path: 'login', flags: ['--context', '--no-wait'], positionals: 1 })
    expect(finish).toEqual({ path: 'login', flags: ['--context', '--resume'], positionals: 1 })
  })

  test('reads with the commands each page maps to', () => {
    const paths = Object.fromEntries(
      examples.map(([kind, subjects]) => [
        kind,
        [...new Set(subjects.flatMap((s) => resolved(s).slice(3).map((c) => c.path)))],
      ]),
    )
    expect(paths).toEqual({
      sandbox: ['sandbox show', 'sandbox fs read', 'sandbox exec', 'sandbox logs'],
      trace: ['traces show'],
      error_group: ['errors show', 'errors events', 'errors event'],
      runtime_logs: ['logs search', 'runtime-logs'],
      session_replay: ['session-replay show', 'session-replay events'],
    })
  })

  test('asks for JSON wherever the prompt does not say the output is plain text', () => {
    const textOnly = new Set(['sandbox fs read', 'sandbox exec', 'sandbox logs', 'runtime-logs'])
    for (const [, subjects] of examples) {
      for (const command of subjects.flatMap((s) => resolved(s).slice(3))) {
        expect(command.flags.includes('--json'), command.path).toBe(!textOnly.has(command.path))
      }
    }
  })
})

describe('the check itself fails on drift', () => {
  const cases: Array<[string, HandoffArg[], RegExp]> = [
    ['a renamed command', ['traces', 'fetch', 'abc'], /is a command group, and "fetch" is not one of its commands/],
    ['a command that does not exist', ['telemetry', 'show'], /the CLI has no command "telemetry"/],
    ['a renamed flag', ['traces', 'show', 'abc', '--project', '4'], /"traces show" has no option --project/],
    ['a dropped global option', ['--context-target', 'x', 'whoami'], /no global option --context-target/],
    ['a missing required flag', ['errors', 'show', '--project-id', '4'], /requires --group-id/],
    ['a missing positional', ['session-replay', 'show', '52'], /got 1 positional argument/],
    ['an extra positional', ['sandbox', 'show', 'sbx_1', 'extra'], /got 2 positional argument/],
    ['a flag that lost its value', ['sandbox', 'fs', 'read', 'sbx_1', '--path'], /--path needs a value/],
  ]
  for (const [name, args, message] of cases) {
    test(name, () => {
      expect(() => resolveAgainstCli(program, args)).toThrow(message)
    })
  }
})
