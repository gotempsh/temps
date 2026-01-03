import type { Command } from 'commander'
import { createProgram } from '../cli.js'
import { colors, newline, header } from '../ui/output.js'

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

function extractCommandInfo(cmd: Command, parentName = ''): CommandInfo {
  const name = parentName ? `${parentName} ${cmd.name()}` : cmd.name()
  const aliases = cmd.aliases()

  const options: OptionInfo[] = cmd.options.map((opt: any) => ({
    flags: opt.flags,
    description: opt.description || '',
    defaultValue: opt.defaultValue !== undefined ? String(opt.defaultValue) : undefined,
    required: opt.required || opt.flags.includes('<'),
  }))

  const subcommands: CommandInfo[] = cmd.commands
    .filter((sub: Command) => sub.name() !== 'docs') // Exclude docs command from docs
    .map((sub: Command) => extractCommandInfo(sub, name))

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

async function generateDocs(options: DocsOptions): Promise<void> {
  // Create a fresh program to extract command structure (excluding docs command)
  const program = createProgram()

  // Extract command information (excluding docs command itself)
  const commands: CommandInfo[] = program.commands
    .filter((cmd: Command) => cmd.name() !== 'docs')
    .map((cmd: Command) => extractCommandInfo(cmd))

  let output: string

  if (options.format === 'json') {
    output = JSON.stringify(commands, null, 2)
  } else {
    const format = options.format === 'mdx' ? 'mdx' : 'markdown'
    output = generateHeader(format)
    output += generateMarkdown(commands)
    output += generateFooter()
  }

  if (options.output) {
    await Bun.write(options.output, output)
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
