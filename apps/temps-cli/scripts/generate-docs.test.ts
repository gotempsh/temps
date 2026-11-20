// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import { registerImportDataCommands } from '../src/commands/services/import-data.js'
import { registerEnvironmentWorkloadCommands } from '../src/commands/environments/workload.js'
import { registerUsersCommands } from '../src/commands/users/index.js'
import { extractCommandInfo, generateMarkdown } from './generate-docs.js'

function fixtureCommand(): Command {
  return new Command('fixture')
    .exitOverride()
    .configureOutput({ writeErr: () => {} })
}

describe('documentation option requirements', () => {
  test('a value-taking option can be omitted but requires a value when supplied', () => {
    const command = fixtureCommand().option('--input <value>', 'Optional input')

    expect(extractCommandInfo(command).options[0]?.required).toBe(false)
    expect(() => command.parse([], { from: 'user' })).not.toThrow()
    expect(() => command.parse(['--input'], { from: 'user' })).toThrow(
      'argument missing',
    )
  })

  test('a mandatory flag remains required when its argument is optional', () => {
    const command = fixtureCommand().requiredOption('--mode [value]', 'Required mode')

    expect(extractCommandInfo(command).options[0]?.required).toBe(true)
    expect(() => command.parse([], { from: 'user' })).toThrow('required option')
    expect(() => command.parse(['--mode'], { from: 'user' })).not.toThrow()
  })

  test('a mandatory flag with a value is documented as required', () => {
    const command = fixtureCommand().requiredOption('--target <name>', 'Target name')

    expect(extractCommandInfo(command).options[0]?.required).toBe(true)
    expect(() => command.parse([], { from: 'user' })).toThrow('required option')
    expect(() => command.parse(['--target', 'example'], { from: 'user' })).not.toThrow()
  })

  test('database import flags retain their actual mandatory and optional contract', () => {
    const services = fixtureCommand().name('services')
    registerImportDataCommands(services)
    const commands = extractCommandInfo(services).subcommands
    const importOptions = new Map(
      commands
        .find((command) => command.name === 'services import-data')
        ?.options.map((option) => [option.flags, option.required]),
    )
    expect(importOptions.get('--id <id>')).toBe(true)
    expect(importOptions.get('--target <name>')).toBe(true)
    for (const flag of [
      '--source-url-env <var>',
      '--source-url <url>',
      '--confirm-target <name>',
      '--timeout <minutes>',
    ]) {
      expect(importOptions.get(flag)).toBe(false)
    }
    const pageOptions = new Map(
      commands
        .find((command) => command.name === 'services import-data-runs')
        ?.options.map((option) => [option.flags, option.required]),
    )
    expect(pageOptions.get('--id <id>')).toBe(true)
    expect(pageOptions.get('--page <n>')).toBe(false)
    expect(pageOptions.get('--page-size <n>')).toBe(false)
  })
})


test('reference headings include required, optional and variadic arguments', () => {
  const command = fixtureCommand().argument('<environment>').argument('[files...]')
  const info = extractCommandInfo(command)
  for (const format of ['markdown', 'mdx'] as const) {
    expect(generateMarkdown([info], 2, format)).toContain('`fixture <environment> [files...]`')
  }
})

test('workload and recovery references match their CLI contracts', () => {
  const env = fixtureCommand().name('env')
  registerEnvironmentWorkloadCommands(env)
  const commands = extractCommandInfo(env)
  const docs = generateMarkdown([commands])
  expect(docs).toContain('`env placement explain <environment>`')
  expect(docs).toContain('`env isolation set <environment> <isolation>`')
  expect(docs).not.toContain('Project slug or ID')
  expect(docs).toContain('only when ingress mode is enforce')
  expect(docs).toContain('login_enforced')
  const volumes = commands.subcommands.find(c => c.name === 'env volumes')!
  for (const [name, flag] of [['create', '--name'], ['attach', '--mount-path']]) {
    const cmd = volumes.subcommands.find(c => c.name === `env volumes ${name}`)!
    expect(cmd.arguments).toContain('<environment>')
    expect(cmd.options.find(o => o.flags.includes(flag!))?.required).toBe(true)
  }
  const root = fixtureCommand()
  registerUsersCommands(root)
  expect(generateMarkdown(extractCommandInfo(root).subcommands)).toContain('Password sign-in is blocked when require_mfa_for_admins applies')
})
