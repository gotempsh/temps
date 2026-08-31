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

  test('keeps the always-loaded skill concise and routes detail to references', async () => {
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')
    const skill = await readFile(join(repositoryRoot, 'skills/temps-cli/SKILL.md'), 'utf8')
    const commandReference = await readFile(
      join(repositoryRoot, 'skills/temps-cli/references/COMMANDS.md'),
      'utf8',
    )

    expect(skill.split('\n').length).toBeLessThanOrEqual(500)
    expect(skill).toContain('[references/COMMANDS.md](references/COMMANDS.md)')
    expect(skill).toContain('[references/WORKFLOWS.md](references/WORKFLOWS.md)')
    expect(skill).toContain('bunx @temps-sdk/cli@0.1.36')
    expect(skill).not.toContain('npm install --global')
    expect(commandReference).toContain('## Command index')
    expect(commandReference).toContain('bunx @temps-sdk/cli@0.1.36 [command]')
    expect(commandReference).toContain('[`backups`](#backups)')
    expect(commandReference).toContain('[`otel-forward`](#otel-forward)')
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.36 --target-context production projects create --name my-app',
    )
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.36 --target-context production environments vars set --project my-app --key DATABASE_URL',
    )
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.36 --target-context production domains add --project my-app --domain app.example.com',
    )
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.36 --target-context production domains remove --project my-app --domain app.example.com',
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
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../../../..')
    const skill = await readFile(join(repositoryRoot, 'skills/temps/SKILL.md'), 'utf8')

    expect(skill.split('\n').length).toBeLessThanOrEqual(500)
    expect(skill).toContain('[references/commands/INDEX.md](references/commands/INDEX.md)')
    expect(skill).toContain('[howtos/observability-onboarding.md](howtos/observability-onboarding.md)')
    expect(skill).toContain('bunx @temps-sdk/cli@0.1.36')
    expect(skill).not.toContain('npm install --global')
  })
})
