import { Command } from 'commander'
import chalk from 'chalk'
import { colors } from './ui/output.js'
import { setQuietMode } from './ui/spinner.js'
import { handleError } from './utils/errors.js'
import { createRequire } from 'module'
import { listContexts, contextExists } from './config/contexts.js'

// Import command modules
import { registerAuthCommands } from './commands/auth/index.js'
import { registerContextCommands } from './commands/context/index.js'
import { registerConfigureCommand } from './commands/configure.js'
import { registerProjectsCommands } from './commands/projects/index.js'
import { registerDeployCommands } from './commands/deploy/index.js'
import { registerDomainsCommands } from './commands/domains/index.js'
import { registerEnvironmentsCommands } from './commands/environments/index.js'
import { registerProvidersCommands } from './commands/providers/index.js'
import { registerBackupsCommands } from './commands/backups/index.js'
import { registerRuntimeLogsCommand } from './commands/runtime-logs.js'
import { registerNotificationsCommands } from './commands/notifications/index.js'
import { registerDnsCommands } from './commands/dns/index.js'
import { registerServicesCommands } from './commands/services/index.js'
import { registerSettingsCommands } from './commands/settings/index.js'
import { registerUsersCommands } from './commands/users/index.js'
import { registerApiKeysCommands } from './commands/apikeys/index.js'
import { registerTeamsCommands } from './commands/teams/index.js'
import { registerMonitorsCommands } from './commands/monitors/index.js'
import { registerWebhooksCommands } from './commands/webhooks/index.js'
import { registerContainersCommands } from './commands/containers/index.js'
import { registerDocsCommand } from './commands/docs.js'
import { registerTokensCommands } from './commands/tokens/index.js'
import { registerErrorsCommands } from './commands/errors/index.js'
import { registerKvCommands } from './commands/kv/index.js'
import { registerFlagsCommands } from './commands/flags/index.js'
import { registerBlobCommands } from './commands/blob/index.js'
import { registerDsnCommands } from './commands/dsn/index.js'
import { registerScansCommands } from './commands/scans/index.js'
import { registerCustomDomainsCommands } from './commands/custom-domains/index.js'
import { registerDnsProvidersCommands } from './commands/dns-providers/index.js'
import { registerIpAccessCommands } from './commands/ip-access/index.js'
import { registerAuditCommands } from './commands/audit/index.js'
import { registerProxyLogsCommands } from './commands/proxy-logs/index.js'
import { registerEmailDomainsCommands } from './commands/email-domains/index.js'
import { registerEmailProvidersCommands } from './commands/email-providers/index.js'
import { registerIncidentsCommands } from './commands/incidents/index.js'
import { registerEmailsCommands } from './commands/emails/index.js'
import { registerLoadBalancerCommands } from './commands/load-balancer/index.js'
import { registerImportsCommands } from './commands/imports/index.js'
import { registerTemplatesCommands } from './commands/templates/index.js'
import { registerPlatformCommands } from './commands/platform/index.js'
import { registerPresetsCommands } from './commands/presets/index.js'
import { registerAnalyticsCommands } from './commands/analytics/index.js'
import { registerFunnelsCommands } from './commands/funnels/index.js'
import { registerNotificationPreferencesCommands } from './commands/notification-preferences/index.js'
import { registerSkillsCommands } from './commands/skills/index.js'
import { registerMcpServersCommands } from './commands/mcp-servers/index.js'
import { registerSecretsCommands } from './commands/secrets/index.js'
import { registerSandboxCommands } from './commands/sandbox/index.js'
import { registerWorkflowCommands } from './commands/workflow/index.js'
import { registerRevenueCommands } from './commands/revenue/index.js'
import { registerSessionReplayCommands } from './commands/session-replay/index.js'

// Developer workflow commands
import { registerInitCommand } from './commands/init/index.js'
import { registerLinkCommand } from './commands/link/index.js'
import { registerUpCommand } from './commands/up/index.js'
import { registerStatusCommand } from './commands/status/index.js'
import { registerAiCommands } from './commands/ai/index.js'
import { registerInstancesCommands } from './commands/instances/index.js'
import { registerEnvSyncCommands } from './commands/env-sync/index.js'
import { registerRollbackCommand } from './commands/rollback/index.js'
import { registerOpenCommand } from './commands/open/index.js'
import { registerExecCommands } from './commands/exec/index.js'
import { registerDevCommand } from './commands/dev/index.js'
import { registerCloudCommands } from './commands/cloud/index.js'

// Read version from package.json
const require = createRequire(import.meta.url)
const pkg = require('../package.json')
const VERSION = pkg.version

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
    .version(VERSION, '-V, --version', 'Display version number')
    .option('--no-color', 'Disable colored output')
    .option('--debug', 'Enable debug output')
    .option(
      '--target-context <name>',
      'Target this context for this command only, without changing the ' +
        'active context (see `temps context list`). Prefer this over ' +
        '`temps context use` / relying on `login`\'s side effect of ' +
        'flipping which context is active — that shared, mutable state is ' +
        'easy to drift out from under you (e.g. across scripts, shells, or ' +
        'agent sessions) and a command then silently runs against the ' +
        'wrong server. Equivalent to setting TEMPS_CONTEXT for just this ' +
        'invocation. Named distinctly from `login`/`logout`\'s own ' +
        '`--context <name>` (which names a context to save/remove, not ' +
        'target) so the two never collide when both could apply to the ' +
        'same invocation.'
    )
    .hook('preAction', async (thisCommand, actionCommand) => {
      const opts = thisCommand.opts()
      if (opts.debug) {
        process.env.DEBUG = '1'
      }
      if (opts.noColor) {
        chalk.level = 0
      }
      if (opts.targetContext) {
        // Validate eagerly and exit rather than letting an unrecognized name
        // silently fall through to whatever the legacy single-instance
        // store (or on-disk `isActive` flag) happens to point at — that
        // silent fallback is exactly the failure mode this flag exists to
        // prevent. A typo in --target-context must be loud, not quietly wrong.
        const contexts = await listContexts()
        if (!contextExists(opts.targetContext, contexts)) {
          const names = contexts.map((c) => c.name).join(', ') || '(none configured)'
          process.stderr.write(
            `Error: --target-context "${opts.targetContext}" does not match any configured context.\n` +
              `Available: ${names}\n` +
              `Run \`temps context list\` to see all contexts, or \`temps login <url>\` to add this one.\n`
          )
          // Hard exit — a preAction hook returning doesn't stop the action
          // handler from still running afterward, and letting an invalid
          // --target-context silently continue (e.g. falling back to the
          // legacy single-instance store) is exactly the failure mode this
          // flag exists to prevent.
          process.exit(1)
        }
        process.env.TEMPS_CONTEXT = opts.targetContext
      }
      // Any leaf command invoked with --json should render machine-readable
      // output only: suppress spinners and other terminal chrome so callers
      // can pipe stdout to `jq` or parse it directly.
      if (actionCommand?.opts().json) {
        setQuietMode(true)
      }
    })

  // Register all command modules
  registerAuthCommands(program)
  registerContextCommands(program)
  registerConfigureCommand(program)
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
  registerTeamsCommands(program)
  registerApiKeysCommands(program)
  registerMonitorsCommands(program)
  registerWebhooksCommands(program)
  registerContainersCommands(program)
  registerTokensCommands(program)
  registerErrorsCommands(program)
  registerKvCommands(program)
registerFlagsCommands(program)
  registerBlobCommands(program)
  registerDsnCommands(program)
  registerScansCommands(program)
  registerCustomDomainsCommands(program)
  registerDnsProvidersCommands(program)
  registerIpAccessCommands(program)
  registerAuditCommands(program)
  registerProxyLogsCommands(program)
  registerEmailDomainsCommands(program)
  registerEmailProvidersCommands(program)
  registerIncidentsCommands(program)
  registerEmailsCommands(program)
  registerLoadBalancerCommands(program)
  registerImportsCommands(program)
  registerTemplatesCommands(program)
  registerPlatformCommands(program)
  registerPresetsCommands(program)
  registerAnalyticsCommands(program)
  registerFunnelsCommands(program)
  registerNotificationPreferencesCommands(program)
  registerSkillsCommands(program)
  registerMcpServersCommands(program)
  registerSecretsCommands(program)
  registerSandboxCommands(program)
  registerWorkflowCommands(program)
  registerRevenueCommands(program)
  registerSessionReplayCommands(program)

  // Developer workflow commands
  registerInitCommand(program)
  registerLinkCommand(program)
  registerUpCommand(program)
  registerStatusCommand(program)
  registerAiCommands(program)
  registerInstancesCommands(program)
  registerEnvSyncCommands(program)
  registerRollbackCommand(program)
  registerOpenCommand(program)
  registerExecCommands(program)
  registerDevCommand(program)
  registerCloudCommands(program)

  registerDocsCommand(program)

  // Custom help
  program.addHelpText('beforeAll', LOGO)

  program.addHelpText(
    'after',
    `
${colors.bold('Quick Start:')}
  ${colors.muted('$')} temps login                    ${colors.muted('# Authenticate with Temps')}
  ${colors.muted('$')} temps init                     ${colors.muted('# Initialize project in current directory')}
  ${colors.muted('$')} temps up                       ${colors.muted('# Deploy from current directory')}
  ${colors.muted('$')} temps status                   ${colors.muted('# View project status')}

${colors.bold('Common Commands:')}
  ${colors.muted('$')} temps link my-app              ${colors.muted('# Link directory to a project')}
  ${colors.muted('$')} temps open                     ${colors.muted('# Open project URL in browser')}
  ${colors.muted('$')} temps rollback                 ${colors.muted('# Rollback to previous deployment')}
  ${colors.muted('$')} temps env:pull                 ${colors.muted('# Pull env vars to .env file')}
  ${colors.muted('$')} temps env:push                 ${colors.muted('# Push .env file to project')}
  ${colors.muted('$')} temps cloud login               ${colors.muted('# Connect to Temps Cloud')}
  ${colors.muted('$')} temps instances list            ${colors.muted('# Manage server instances')}

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
