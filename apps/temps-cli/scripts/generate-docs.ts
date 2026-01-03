#!/usr/bin/env bun
/**
 * CLI Documentation Generator
 *
 * Generates markdown/mdx documentation for all temps CLI commands.
 *
 * Usage:
 *   bun run scripts/generate-docs.ts > docs/CLI.md
 *   bun run scripts/generate-docs.ts --output docs/CLI.md
 *   bun run scripts/generate-docs.ts --format markdown
 *   bun run scripts/generate-docs.ts --format mdx
 *   bun run scripts/generate-docs.ts --format json
 */

import { Command } from 'commander'
import { registerProjectsCommands } from '../src/commands/projects/index.js'
import { registerDeployCommands } from '../src/commands/deploy/index.js'
import { registerDomainsCommands } from '../src/commands/domains/index.js'
import { registerEnvironmentsCommands } from '../src/commands/environments/index.js'
import { registerProvidersCommands } from '../src/commands/providers/index.js'
import { registerBackupsCommands } from '../src/commands/backups/index.js'
import { registerRuntimeLogsCommand } from '../src/commands/runtime-logs.js'
import { registerNotificationsCommands } from '../src/commands/notifications/index.js'
import { registerDnsCommands } from '../src/commands/dns/index.js'
import { registerServicesCommands } from '../src/commands/services/index.js'
import { registerSettingsCommands } from '../src/commands/settings/index.js'
import { registerUsersCommands } from '../src/commands/users/index.js'
import { registerApiKeysCommands } from '../src/commands/apikeys/index.js'
import { registerMonitorsCommands } from '../src/commands/monitors/index.js'
import { registerWebhooksCommands } from '../src/commands/webhooks/index.js'
import { registerContainersCommands } from '../src/commands/containers/index.js'

interface CommandInfo {
  name: string
  description: string
  aliases: string[]
  options: OptionInfo[]
  subcommands: CommandInfo[]
  usage?: string
}

interface OptionInfo {
  flags: string
  description: string
  defaultValue?: string
  required: boolean
}

interface DocsFormat {
  format: 'markdown' | 'mdx' | 'json'
  output?: string
}

function parseArgs(): DocsFormat {
  const args = process.argv.slice(2)
  const result: DocsFormat = { format: 'markdown' }

  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--format' && args[i + 1]) {
      result.format = args[i + 1] as 'markdown' | 'mdx' | 'json'
      i++
    } else if (args[i] === '--output' && args[i + 1]) {
      result.output = args[i + 1]
      i++
    }
  }

  return result
}

function extractCommandInfo(cmd: Command, parentName = ''): CommandInfo {
  const name = parentName ? `${parentName} ${cmd.name()}` : cmd.name()
  const aliases = cmd.aliases()

  const options: OptionInfo[] = cmd.options.map((opt: any) => ({
    flags: opt.flags,
    description: opt.description || '',
    defaultValue: opt.defaultValue !== undefined ? String(opt.defaultValue) : undefined,
    required: opt.required || opt.flags.includes('<'),
  }))

  const subcommands: CommandInfo[] = cmd.commands.map((sub: Command) =>
    extractCommandInfo(sub, name)
  )

  return {
    name,
    description: cmd.description() || '',
    aliases,
    options,
    subcommands,
    usage: cmd.usage(),
  }
}

function generateMarkdown(commands: CommandInfo[], level = 2): string {
  let md = ''

  for (const cmd of commands) {
    const heading = '#'.repeat(level)
    const aliasText = cmd.aliases.length > 0 ? ` (alias: \`${cmd.aliases.join('`, `')}\`)` : ''

    md += `${heading} \`${cmd.name}\`${aliasText}\n\n`

    if (cmd.description) {
      md += `${cmd.description}\n\n`
    }

    if (cmd.options.length > 0) {
      md += '**Options:**\n\n'
      md += '| Flag | Description | Default | Required |\n'
      md += '|------|-------------|---------|----------|\n'

      for (const opt of cmd.options) {
        const defaultVal = opt.defaultValue !== undefined ? `\`${opt.defaultValue}\`` : '-'
        const required = opt.required ? 'Yes' : 'No'
        const escapedDesc = opt.description.replace(/\|/g, '\\|')
        md += `| \`${opt.flags}\` | ${escapedDesc} | ${defaultVal} | ${required} |\n`
      }
      md += '\n'
    }

    if (cmd.subcommands.length > 0) {
      md += '**Subcommands:**\n\n'
      for (const sub of cmd.subcommands) {
        const subAliases = sub.aliases.length > 0 ? ` (\`${sub.aliases.join('`, `')}\`)` : ''
        md += `- \`${sub.name.split(' ').pop()}\`${subAliases} - ${sub.description}\n`
      }
      md += '\n'

      // Generate detailed docs for subcommands
      md += generateMarkdown(cmd.subcommands, level + 1)
    }
  }

  return md
}

function generateHeader(format: 'markdown' | 'mdx' = 'markdown'): string {
  const date = new Date().toISOString().split('T')[0]

  const mdxMetadata = `export const metadata = {
  title: 'CLI Reference',
  description: 'Complete reference for the Temps CLI - all commands, options, and usage examples.',
}

`

  return `${format === 'mdx' ? mdxMetadata : ''}# Temps CLI Reference

> Auto-generated documentation for the Temps CLI.
>
> Generated on: ${date}

## Installation

\`\`\`bash
# Install globally
npm install -g @temps/cli

# Or use with npx
npx @temps/cli [command]
\`\`\`

## Authentication

Before using most commands, you need to authenticate:

\`\`\`bash
# Login with API key
temps login

# Or configure with wizard
temps configure
\`\`\`

## Global Options

| Flag | Description |
|------|-------------|
| \`-v, --version\` | Display version number |
| \`--no-color\` | Disable colored output |
| \`--debug\` | Enable debug output |
| \`-h, --help\` | Display help for command |

## Commands

`
}

function generateFooter(): string {
  return `
---

## Examples

### Basic Workflow

\`\`\`bash
# Login to Temps
temps login

# Create a new project
temps projects create --name my-app

# Deploy to production
temps deploy --project my-app --environment production

# View deployment logs
temps logs --project my-app --follow

# Stream runtime container logs
temps runtime-logs --project my-app

# List containers
temps containers list --project-id 1 --environment-id 1
\`\`\`

### Managing Environments

\`\`\`bash
# List environments
temps environments list --project my-app

# Set environment variables
temps environments vars set --project my-app --key DATABASE_URL --value "postgres://..."

# View environment variables
temps environments vars list --project my-app
\`\`\`

### Managing Domains

\`\`\`bash
# Add a custom domain
temps domains add --project my-app --domain app.example.com

# List domains
temps domains list --project my-app

# Remove a domain
temps domains remove --project my-app --domain app.example.com
\`\`\`

## Environment Variables

The CLI respects the following environment variables:

| Variable | Description |
|----------|-------------|
| \`TEMPS_API_URL\` | API endpoint URL |
| \`TEMPS_API_TOKEN\` | API authentication token |
| \`TEMPS_API_KEY\` | API key (alternative to token) |
| \`NO_COLOR\` | Disable colored output |

## Configuration

Configuration is stored in:
- **Config file**: \`~/.temps/config.json\`
- **Credentials**: \`~/.temps/.secrets\`

Use \`temps configure show\` to view current configuration.

## Support

- Documentation: https://temps.dev/docs
- Issues: https://github.com/kfs/temps/issues
`
}

async function main() {
  const args = parseArgs()

  // Create a fresh program to extract command structure
  const program = new Command()
  program
    .name('temps')
    .description('CLI for Temps deployment platform')
    .version('0.0.0')

  // Register all commands
  registerProjectsCommands(program)
  registerDeployCommands(program)
  registerDomainsCommands(program)
  registerEnvironmentsCommands(program)
  registerProvidersCommands(program)
  registerBackupsCommands(program)
  registerRuntimeLogsCommand(program)
  registerNotificationsCommands(program)
  registerDnsCommands(program)
  registerServicesCommands(program)
  registerSettingsCommands(program)
  registerUsersCommands(program)
  registerApiKeysCommands(program)
  registerMonitorsCommands(program)
  registerWebhooksCommands(program)
  registerContainersCommands(program)

  // Extract command information
  const commands: CommandInfo[] = program.commands.map((cmd: Command) =>
    extractCommandInfo(cmd)
  )

  let output: string

  if (args.format === 'json') {
    output = JSON.stringify(commands, null, 2)
  } else {
    const format = args.format === 'mdx' ? 'mdx' : 'markdown'
    output = generateHeader(format)
    output += generateMarkdown(commands)
    output += generateFooter()
  }

  if (args.output) {
    await Bun.write(args.output, output)
    console.error(`Documentation written to ${args.output}`)
  } else {
    console.log(output)
  }
}

main().catch(console.error)
