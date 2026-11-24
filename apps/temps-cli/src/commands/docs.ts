// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { writeFile } from 'node:fs/promises'
import { createProgram } from '../cli.js'
import { colors, newline, header } from '../ui/output.js'

interface CommandInfo {
  name: string
  description: string
  aliases: string[]
  options: OptionInfo[]
  subcommands: CommandInfo[]
  /** Positional arguments as Commander shows them: `<release>`, `[path...]`. */
  arguments: string
  usage?: string
}

interface OptionInfo {
  flags: string
  description: string
  defaultValue?: string
  required: boolean
}

interface DocsOptions {
  format?: 'markdown' | 'mdx' | 'json'
  output?: string
}

export function registerDocsCommand(program: Command): void {
  program
    .command('docs')
    .description('Generate CLI documentation')
    .option('-f, --format <format>', 'Output format (markdown, mdx, json)', 'markdown')
    .option('-o, --output <file>', 'Write output to file')
    .action(generateDocs)
}

/**
 * Hidden commands (registered but not implemented yet) are left out of the
 * docs for the same reason they are left out of `--help`: a reference must
 * not advertise a command that only prints "coming soon".
 */
function isDocumented(cmd: Command): boolean {
  return !(cmd as Command & { _hidden?: boolean })._hidden
}

function extractCommandInfo(cmd: Command, parentName = ''): CommandInfo {
  const name = parentName ? `${parentName} ${cmd.name()}` : cmd.name()
  const aliases = cmd.aliases()

  const options: OptionInfo[] = cmd.options.map((opt: any) => ({
    flags: opt.flags,
    description: opt.description || '',
    defaultValue: opt.defaultValue !== undefined ? String(opt.defaultValue) : undefined,
    // Commander uses `<value>` to mean the value is required *when the option
    // is supplied*. It does not make the option itself mandatory. Only
    // `.makeOptionMandatory()` sets the `mandatory` flag.
    required: opt.mandatory === true,
  }))

  const subcommands: CommandInfo[] = cmd.commands
    .filter((sub: Command) => sub.name() !== 'docs') // Exclude docs command from docs
    .filter(isDocumented)
    .map((sub: Command) => extractCommandInfo(sub, name))

  return {
    name,
    description: cmd.description() || '',
    aliases,
    options,
    subcommands,
    arguments: commandArguments(cmd),
    usage: cmd.usage(),
  }
}

/** A command's positional arguments, `<required>` and `[optional]`. */
export function commandArguments(cmd: Command): string {
  return cmd.registeredArguments
    .map((arg) => {
      const name = `${arg.name()}${arg.variadic ? '...' : ''}`
      return arg.required ? `<${name}>` : `[${name}]`
    })
    .join(' ')
}

function escapeForMdx(text: string): string {
  // Wrap substrings containing curly braces in backticks so MDX
  // doesn't interpret them as JSX expressions.
  return text.replace(/('[^']*\{[^']*')/g, '`$1`')
}

function generateMarkdown(commands: CommandInfo[], level = 2, format: 'markdown' | 'mdx' = 'markdown'): string {
  let md = ''

  for (const cmd of commands) {
    const heading = '#'.repeat(level)
    const aliasText = cmd.aliases.length > 0 ? ` (alias: \`${cmd.aliases.join('`, `')}\`)` : ''

    md += `${heading} \`${cmd.name}\`${aliasText}\n\n`

    if (cmd.description) {
      md += `${cmd.description}\n\n`
    }

    // Headings stay the bare command name (they name the routed reference
    // files and anchors), so positional arguments are shown here.
    if (cmd.arguments) {
      const options = cmd.options.length > 0 ? ' [options]' : ''
      md += `**Usage:** \`${cmd.name} ${cmd.arguments}${options}\`\n\n`
    }

    if (cmd.options.length > 0) {
      md += '**Options:**\n\n'
      md += '| Flag | Description | Default | Required |\n'
      md += '|------|-------------|---------|----------|\n'

      for (const opt of cmd.options) {
        const defaultVal = opt.defaultValue !== undefined ? `\`${opt.defaultValue}\`` : '-'
        const required = opt.required ? 'Yes' : 'No'
        let escapedDesc = opt.description.replace(/\|/g, '\\|')
        if (format === 'mdx') {
          escapedDesc = escapeForMdx(escapedDesc)
        }
        md += `| \`${opt.flags.replace(/\|/g, '\\|')}\` | ${escapedDesc} | ${defaultVal} | ${required} |\n`
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
      md += generateMarkdown(cmd.subcommands, level + 1, format)
    }
  }

  return md
}

function markdownAnchor(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, '')
    .trim()
    .replace(/\s+/g, '-')
}

function generateCommandIndex(commands: CommandInfo[]): string {
  let index = '## Command index\n\n'
  index += 'Use this index or search for a top-level command heading to load only the relevant group.\n\n'

  for (const command of commands) {
    index += `- [\`${command.name}\`](#${markdownAnchor(command.name)}) - ${command.description}\n`
  }

  return `${index}\n## Commands\n\n`
}

/**
 * The npm dist-tag that publishes `version`: a prerelease ships on `beta`, a
 * release on `latest`. The examples use it so they run the CLI the reference
 * documents, and SDK release preparation moves the skills' pins with it.
 */
export function npmChannel(version: string): 'beta' | 'latest' {
  return version.includes('-') ? 'beta' : 'latest'
}

function generateHeader(
  format: 'markdown' | 'mdx' = 'markdown',
  version: string,
): string {
  const channel = npmChannel(version)
  const mdxMetadata = `export const metadata = {
  title: 'CLI Reference',
  description: 'Complete reference for the Temps CLI - all commands, options, and usage examples.',
}

`

  return `${format === 'mdx' ? mdxMetadata : ''}# Temps CLI Reference

> Auto-generated documentation for the Temps CLI.
>
> Generated from: \`@temps-sdk/cli@${version}\`
>
> Apply the authorization, target-context, and secret-handling rules in
> [the Temps CLI skill](../SKILL.md) before executing a command.

## Installation

\`\`\`bash
bunx @temps-sdk/cli@${channel} [command]

# Fallback when Bun is unavailable
npx @temps-sdk/cli@${channel} [command]
\`\`\`

## Authentication

Before using most commands, you need to authenticate:

\`\`\`bash
# Login interactively
bunx @temps-sdk/cli@${channel} login

# Or configure with wizard
bunx @temps-sdk/cli@${channel} configure
\`\`\`

## Global Options

| Flag | Description |
|------|-------------|
| \`-V, --version\` | Display version number |
| \`--target-context <name>\` | Target one configured server for this invocation |
| \`--no-color\` | Disable colored output |
| \`--debug\` | Enable debug output |
| \`-h, --help\` | Display help for command |

`
}

function generateFooter(version: string): string {
  const channel = npmChannel(version)
  return `
---

## Examples

### Basic Workflow

\`\`\`bash
# Login to Temps
bunx @temps-sdk/cli@${channel} login

# Create a new project on the intended server
bunx @temps-sdk/cli@${channel} --target-context production projects create --name my-app

# Deploy to production
bunx @temps-sdk/cli@${channel} --target-context production deploy --project my-app --environment production

# View deployment logs
bunx @temps-sdk/cli@${channel} deployments logs --project my-app --follow

# Stream runtime container logs
bunx @temps-sdk/cli@${channel} runtime-logs --project my-app

# List containers
bunx @temps-sdk/cli@${channel} containers list --project-id 1 --environment-id 1
\`\`\`

### Managing Environments

\`\`\`bash
# List environments
bunx @temps-sdk/cli@${channel} environments list --project my-app

# Set an environment variable on the intended server (prompts for the value)
bunx @temps-sdk/cli@${channel} --target-context production environments vars set --project my-app DATABASE_URL --environments production

# Store a secret from a file, or pipe it from a password manager (never type it on the command line)
bunx @temps-sdk/cli@${channel} --target-context production environments vars set --project my-app DB_PASSWORD --secret --value-file ./db-password.txt --environments production
op read --no-newline op://vault/db/password | bunx @temps-sdk/cli@${channel} --target-context production environments vars set --project my-app DB_PASSWORD --secret --value-stdin --environments production

# View environment variables
bunx @temps-sdk/cli@${channel} environments vars list --project my-app
\`\`\`

### Managing Domains

\`\`\`bash
# Request a certificate for a domain on the intended server
bunx @temps-sdk/cli@${channel} --target-context production domains add --domain app.example.com

# Route the domain to a project environment
bunx @temps-sdk/cli@${channel} --target-context production custom-domains create --project-id 1 --environment-id 1 --domain app.example.com

# List domains
bunx @temps-sdk/cli@${channel} domains list

# Remove a domain from the intended server
bunx @temps-sdk/cli@${channel} --target-context production domains remove --domain app.example.com
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
- **Credentials**: Stored securely in \`~/.temps/\` with restricted file permissions

Use \`bunx @temps-sdk/cli@${channel} configure show\` to view current configuration.

## Support

- Documentation: https://temps.sh/docs
- Issues: https://github.com/gotempsh/temps/issues
`
}

export async function generateDocs(options: DocsOptions): Promise<void> {
  // Create a fresh program to extract command structure (excluding docs command)
  const program = createProgram()

  // Extract command information (excluding docs command itself)
  const commands: CommandInfo[] = program.commands
    .filter((cmd: Command) => cmd.name() !== 'docs')
    .filter(isDocumented)
    .map((cmd: Command) => extractCommandInfo(cmd))

  let output: string

  if (options.format === 'json') {
    output = JSON.stringify(commands, null, 2)
  } else {
    const format = options.format === 'mdx' ? 'mdx' : 'markdown'
    const version = program.version() ?? 'unknown'
    output = generateHeader(format, version)
    output += generateCommandIndex(commands)
    output += generateMarkdown(commands, 2, format)
    output += generateFooter(version)
  }

  if (options.output) {
    // The published CLI is bundled for Node. Using Bun.write here made the
    // `temps docs --output ...` command crash for npm users even though the
    // development and test runtime is Bun.
    await writeFile(options.output, output, 'utf8')
    newline()
    header('Documentation Generated')
    console.log(`${colors.success('Written to:')} ${options.output}`)
    console.log(`${colors.muted('Format:')} ${options.format || 'markdown'}`)
    console.log(`${colors.muted('Commands documented:')} ${commands.length}`)
    newline()
  } else {
    console.log(output)
  }
}
