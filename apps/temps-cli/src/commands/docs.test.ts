// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { execFile } from 'node:child_process'
import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { generateDocs } from './docs.js'

const tempDirs: string[] = []
const execFileAsync = promisify(execFile)

afterEach(async () => {
  await Promise.all(tempDirs.splice(0).map((path) => rm(path, { recursive: true, force: true })))
})

async function cliChannel(): Promise<string> {
  const { createProgram } = await import('../cli.js')
  const { npmChannel } = await import('./docs.js')
  return npmChannel(createProgram().version() ?? '')
}

describe('generateDocs', () => {
  test('writes the complete command catalog without requiring Bun globals', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'temps-cli-docs-'))
    tempDirs.push(directory)
    const output = join(directory, 'commands.json')

    await generateDocs({ format: 'json', output })

    const commands = JSON.parse(await readFile(output, 'utf8')) as Array<{ name: string }>
    expect(commands.some((command) => command.name === 'otel-forward')).toBe(true)
    expect(commands.some((command) => command.name === 'projects')).toBe(true)
    expect(commands.some((command) => command.name === 'cloud')).toBe(true)
    // kv and blob are implemented now, so the docs list them.
    expect(commands.some((command) => command.name === 'kv')).toBe(true)
    expect(commands.some((command) => command.name === 'blob')).toBe(true)
    // Registered but not implemented: neither help nor docs advertise them.
    for (const unavailable of ['exec', 'dev']) {
      expect(commands.some((command) => command.name === unavailable)).toBe(false)
    }
  })

  test('shows the positional arguments a command requires', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'temps-cli-docs-'))
    tempDirs.push(directory)
    const output = join(directory, 'COMMANDS.md')

    await generateDocs({ format: 'markdown', output })

    const markdown = await readFile(output, 'utf8')
    const section = markdown.slice(markdown.indexOf('### `errors release`\n'))
    // The heading stays the bare name; the argument is in the usage line.
    expect(section.split('\n### ')[0]).toContain(
      '**Usage:** `errors release <release> [options]`',
    )
    const { createProgram } = await import('../cli.js')
    const { commandArguments } = await import('./docs.js')
    const program = createProgram()
    const errors = program.commands.find((command) => command.name() === 'errors')
    const release = errors?.commands.find((command) => command.name() === 'release')
    expect(release && commandArguments(release)).toBe('<release>')
    const releases = errors?.commands.find((command) => command.name() === 'releases')
    expect(releases && commandArguments(releases)).toBe('')
  })

  test('hides unimplemented commands from help but keeps them runnable', async () => {
    const { createProgram } = await import('../cli.js')
    const program = createProgram()
    const help = program.helpInformation()
    for (const available of ['kv', 'blob']) {
      expect(help).toMatch(new RegExp(`^\\s+${available}\\b`, 'm'))
    }
    for (const unavailable of ['exec', 'dev']) {
      expect(help).not.toMatch(new RegExp(`^\\s+${unavailable}\\b`, 'm'))
      expect(program.commands.some((command) => command.name() === unavailable)).toBe(
        true,
      )
    }
  })

  test('keeps the skill command appendix synchronized with the CLI', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'temps-cli-docs-'))
    tempDirs.push(directory)
    const output = join(directory, 'COMMANDS.md')
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')
    const committedReference = join(
      repositoryRoot,
      'skills/temps-cli/references/COMMANDS.md',
    )

    await generateDocs({ format: 'markdown', output })

    expect(await readFile(output, 'utf8')).toBe(await readFile(committedReference, 'utf8'))
  })

  test('pins examples to the dist-tag that publishes the CLI version', async () => {
    const { npmChannel } = await import('./docs.js')
    expect(npmChannel('0.1.38')).toBe('latest')
    expect(npmChannel('1.0.0')).toBe('latest')
    expect(npmChannel('0.2.0-beta.1')).toBe('beta')
  })

  test('keeps the always-loaded skill concise and routes detail to references', async () => {
    const channel = await cliChannel()
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')
    const skill = await readFile(join(repositoryRoot, 'skills/temps-cli/SKILL.md'), 'utf8')
    const commandReference = await readFile(
      join(repositoryRoot, 'skills/temps-cli/references/COMMANDS.md'),
      'utf8',
    )

    expect(skill.split('\n').length).toBeLessThanOrEqual(500)
    expect(skill).toContain('[references/COMMANDS.md](references/COMMANDS.md)')
    expect(skill).toContain('[references/WORKFLOWS.md](references/WORKFLOWS.md)')
    expect(skill).toContain(`bunx @temps-sdk/cli@${channel}`)
    expect(skill).not.toContain('npm install --global')
    expect(commandReference).toContain('## Command index')
    expect(commandReference).toContain(`bunx @temps-sdk/cli@${channel} [command]`)
    expect(commandReference).toContain('[`backups`](#backups)')
    expect(commandReference).toContain('[`otel-forward`](#otel-forward)')
    expect(commandReference).toContain(
      `bunx @temps-sdk/cli@${channel} --target-context production projects create --name my-app`,
    )
    expect(commandReference).toContain(
      `bunx @temps-sdk/cli@${channel} --target-context production environments vars set --project my-app DATABASE_URL --environments production`,
    )
    expect(commandReference).toContain(
      `bunx @temps-sdk/cli@${channel} --target-context production domains add --domain app.example.com`,
    )
    expect(commandReference).toContain(
      `bunx @temps-sdk/cli@${channel} --target-context production domains remove --domain app.example.com`,
    )
    expect(commandReference).toContain('| `-n, --limit <number>` | Limit results | `10` | No |')
  })

  test('keeps the routed Temps command references synchronized', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'temps-skill-docs-'))
    tempDirs.push(directory)
    const source = join(directory, 'COMMANDS.md')
    const destination = join(directory, 'commands')
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')

    await generateDocs({ format: 'markdown', output: source })
    await execFileAsync('python3', [
      join(repositoryRoot, 'skills/temps/scripts/generate_command_references.py'),
      '--source',
      source,
      '--destination',
      destination,
    ])

    const committed = join(repositoryRoot, 'skills/temps/references/commands')
    const generatedFiles = (await readdir(destination)).sort()
    expect(generatedFiles).toEqual((await readdir(committed)).sort())
    for (const filename of generatedFiles) {
      expect(await readFile(join(destination, filename), 'utf8')).toBe(
        await readFile(join(committed, filename), 'utf8'),
      )
    }
  })

  test('keeps the Temps router concise and focused', async () => {
    const channel = await cliChannel()
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')
    const skill = await readFile(join(repositoryRoot, 'skills/temps/SKILL.md'), 'utf8')

    expect(skill.split('\n').length).toBeLessThanOrEqual(500)
    expect(skill).toContain('[references/commands/INDEX.md](references/commands/INDEX.md)')
    expect(skill).toContain('[howtos/observability-onboarding.md](howtos/observability-onboarding.md)')
    expect(skill).toContain(`bunx @temps-sdk/cli@${channel}`)
    expect(skill).not.toContain('npm install --global')
  })
})
