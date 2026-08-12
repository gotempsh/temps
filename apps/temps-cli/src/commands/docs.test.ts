import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { generateDocs } from './docs.js'

const tempDirs: string[] = []

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
    expect(skill).toContain('bunx @temps-sdk/cli@0.1.31')
    expect(skill).not.toContain('npm install --global')
    expect(commandReference).toContain('## Command index')
    expect(commandReference).toContain('bunx @temps-sdk/cli@0.1.31 [command]')
    expect(commandReference).toContain('[`backups`](#backups)')
    expect(commandReference).toContain('[`otel-forward`](#otel-forward)')
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.31 --target-context production projects create --name my-app',
    )
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.31 --target-context production environments vars set --project my-app --key DATABASE_URL',
    )
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.31 --target-context production domains add --project my-app --domain app.example.com',
    )
    expect(commandReference).toContain(
      'bunx @temps-sdk/cli@0.1.31 --target-context production domains remove --project my-app --domain app.example.com',
    )
  })
})
