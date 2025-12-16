import { Command } from 'commander'
import chalk from 'chalk'
import { colors, icons } from './ui/output.js'
import { handleError } from './utils/errors.js'

// Import command modules
import { registerAuthCommands } from './commands/auth/index.js'
import { registerConfigureCommand } from './commands/configure.js'
import { registerProjectsCommands } from './commands/projects/index.js'
import { registerDeployCommands } from './commands/deploy/index.js'
import { registerDomainsCommands } from './commands/domains/index.js'
import { registerEnvironmentsCommands } from './commands/environments/index.js'
import { registerProvidersCommands } from './commands/providers/index.js'
import { registerBackupsCommands } from './commands/backups/index.js'
import { registerRuntimeLogsCommand } from './commands/runtime-logs.js'

const VERSION = '0.1.0'

const LOGO = `
${chalk.cyan('╔════════════════════════════════════════╗')}
${chalk.cyan('║')}  ${chalk.bold.white('⚡ TEMPS CLI')}                          ${chalk.cyan('║')}
${chalk.cyan('║')}  ${chalk.gray('Deployment Platform for Modern Apps')}   ${chalk.cyan('║')}
${chalk.cyan('╚════════════════════════════════════════╝')}
`

export function createProgram(): Command {
  const program = new Command()

  program
    .name('temps')
    .description('CLI for Temps deployment platform')
    .version(VERSION, '-v, --version', 'Display version number')
    .option('--no-color', 'Disable colored output')
    .option('--debug', 'Enable debug output')
    .hook('preAction', (thisCommand) => {
      const opts = thisCommand.opts()
      if (opts.debug) {
        process.env.DEBUG = '1'
      }
      if (opts.noColor) {
        chalk.level = 0
      }
    })

  // Register all command modules
  registerAuthCommands(program)
  registerConfigureCommand(program)
  registerProjectsCommands(program)
  registerDeployCommands(program)
  registerDomainsCommands(program)
  registerEnvironmentsCommands(program)
  registerProvidersCommands(program)
  registerBackupsCommands(program)
  registerRuntimeLogsCommand(program)

  // Custom help
  program.addHelpText('beforeAll', LOGO)

  program.addHelpText(
    'after',
    `
${colors.bold('Examples:')}
  ${colors.muted('$')} temps login                    ${colors.muted('# Authenticate with Temps')}
  ${colors.muted('$')} temps configure                ${colors.muted('# Configure CLI settings')}
  ${colors.muted('$')} temps projects list            ${colors.muted('# List all projects')}
  ${colors.muted('$')} temps deploy my-app            ${colors.muted('# Deploy a project')}
  ${colors.muted('$')} temps logs my-app --follow     ${colors.muted('# Stream deployment logs')}
  ${colors.muted('$')} temps env vars my-app list     ${colors.muted('# List environment variables')}

${colors.bold('Documentation:')}
  ${colors.primary('https://temps.dev/docs')}

${colors.bold('Support:')}
  ${colors.primary('https://github.com/kfs/temps/issues')}
`
  )

  return program
}

export async function run(): Promise<void> {
  const program = createProgram()

  try {
    await program.parseAsync(process.argv)
  } catch (error) {
    handleError(error)
  }
}
