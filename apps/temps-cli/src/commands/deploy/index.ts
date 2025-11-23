import type { Command } from 'commander'
import { deploy } from './deploy.js'
import { list } from './list.js'
import { logs } from './logs.js'
import { rollback } from './rollback.js'
import { status } from './status.js'

export function registerDeployCommands(program: Command): void {
  // Main deploy command
  program
    .command('deploy [project]')
    .description('Deploy a project')
    .option('-e, --environment <env>', 'Target environment', 'production')
    .option('-b, --branch <branch>', 'Git branch to deploy')
    .option('--no-wait', 'Do not wait for deployment to complete')
    .action(deploy)

  // Deployments subcommand
  const deployments = program
    .command('deployments')
    .alias('deploys')
    .description('Manage deployments')

  deployments
    .command('list [project]')
    .alias('ls')
    .description('List deployments')
    .option('-e, --environment <env>', 'Filter by environment')
    .option('-n, --limit <number>', 'Limit results', '10')
    .option('--json', 'Output in JSON format')
    .action(list)

  deployments
    .command('status <deployment>')
    .description('Show deployment status')
    .option('--json', 'Output in JSON format')
    .action(status)

  deployments
    .command('rollback <project>')
    .description('Rollback to previous deployment')
    .option('-e, --environment <env>', 'Target environment', 'production')
    .option('--to <deployment>', 'Rollback to specific deployment ID')
    .action(rollback)

  // Logs command at root level
  program
    .command('logs <project>')
    .description('Stream deployment logs')
    .option('-e, --environment <env>', 'Environment', 'production')
    .option('-f, --follow', 'Follow log output')
    .option('-n, --lines <number>', 'Number of lines to show', '100')
    .option('--deployment <id>', 'Specific deployment ID')
    .action(logs)
}
