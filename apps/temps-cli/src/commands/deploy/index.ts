import type { Command } from 'commander'
import { deploy } from './deploy.js'
import { list } from './list.js'
import { logs } from './logs.js'
import { rollback } from './rollback.js'
import { status } from './status.js'
import { cancelDeploymentAction, pauseDeploymentAction, resumeDeploymentAction, teardownDeploymentAction } from './actions.js'

export function registerDeployCommands(program: Command): void {
  // Main deploy command
  program
    .command('deploy')
    .description('Deploy a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <env>', 'Target environment name')
    .option('--environment-id <id>', 'Target environment ID')
    .option('-b, --branch <branch>', 'Git branch to deploy')
    .option('--no-wait', 'Do not wait for deployment to complete')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(deploy)

  // Deployments subcommand
  const deployments = program
    .command('deployments')
    .alias('deploys')
    .description('Manage deployments')

  deployments
    .command('list')
    .alias('ls')
    .description('List deployments')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <env>', 'Filter by environment')
    .option('-n, --limit <number>', 'Limit results', '10')
    .option('--json', 'Output in JSON format')
    .action(list)

  deployments
    .command('status')
    .description('Show deployment status')
    .option('-p, --project <project>', 'Project slug or ID (required)')
    .option('-d, --deployment-id <id>', 'Deployment ID (required)')
    .option('--json', 'Output in JSON format')
    .action(status)

  deployments
    .command('rollback')
    .description('Rollback to previous deployment')
    .option('-p, --project <project>', 'Project slug or ID (required)')
    .option('-e, --environment <env>', 'Target environment', 'production')
    .option('--to <deployment>', 'Rollback to specific deployment ID')
    .action(rollback)

  deployments
    .command('cancel')
    .description('Cancel a running deployment')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-d, --deployment-id <id>', 'Deployment ID')
    .option('-f, --force', 'Skip confirmation')
    .action(cancelDeploymentAction)

  deployments
    .command('pause')
    .description('Pause a deployment')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-d, --deployment-id <id>', 'Deployment ID')
    .action(pauseDeploymentAction)

  deployments
    .command('resume')
    .description('Resume a paused deployment')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-d, --deployment-id <id>', 'Deployment ID')
    .action(resumeDeploymentAction)

  deployments
    .command('teardown')
    .description('Teardown a deployment and remove all resources')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-d, --deployment-id <id>', 'Deployment ID')
    .option('-f, --force', 'Skip confirmation')
    .action(teardownDeploymentAction)

  // Logs command at root level
  program
    .command('logs')
    .description('Stream deployment logs')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <env>', 'Environment', 'production')
    .option('-f, --follow', 'Follow log output')
    .option('-n, --lines <number>', 'Number of lines to show', '100')
    .option('-d, --deployment <id>', 'Specific deployment ID')
    .action(logs)
}
