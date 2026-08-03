import type { Command } from 'commander'
import { list } from './list.js'
import { create } from './create.js'
import { show } from './show.js'
import { remove } from './delete.js'
import { updateProjectAction, updateSettingsAction, updateGitAction, updateConfigAction } from './update.js'
import { registerProjectSecretsCommands } from './secrets.js'

export function registerProjectsCommands(program: Command): void {
  const projects = program
    .command('projects')
    .alias('project')
    .alias('p')
    .description('Manage projects')

  registerProjectSecretsCommands(projects)

  projects
    .command('list')
    .alias('ls')
    .description('List all projects')
    .option('--json', 'Output in JSON format')
    .option('--page <n>', 'Page number')
    .option('--per-page <n>', 'Items per page')
    .action(list)

  projects
    .command('create')
    .alias('new')
    .description('Create a new project (git-based or manual deployment)')
    .option('-n, --name <name>', 'Project name')
    .option('-d, --description <description>', 'Project description')
    .option('--repo <repository>', 'Repository in owner/name format (nested groups supported: group/subgroup/name)')
    .option('--branch <branch>', 'Git branch')
    .option('--directory <directory>', 'Root directory (relative to repo)')
    .option('--preset <preset>', 'Build preset (e.g., nextjs, nodejs, static, docker)')
    .option('--connection <id>', 'Git connection ID')
    .option('--manual', 'Create a manual (non-git) project - deploy via Docker image or static files')
    .option(
      '--source-type <type>',
      'Manual deployment method: manual (flexible), docker_image, or static_files'
    )
    .option('--image <image>', 'Docker image for the first deployment (manual mode)')
    .option('--port <port>', 'Application/container port (manual mode, default: 3000)')
    .option('-y, --yes', 'Skip optional prompts (services, env vars, set-default)')
    .action(create)

  projects
    .command('show')
    .alias('get')
    .description('Show project details')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(show)

  projects
    .command('update')
    .alias('edit')
    .description('Update project name and description')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-n, --name <name>', 'New project name')
    .option('-d, --description <description>', 'New project description')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts, use provided values (for automation)')
    .action(updateProjectAction)

  projects
    .command('settings')
    .description('Update project settings (slug, attack mode, preview environments, image retention)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--slug <slug>', 'Project URL slug')
    .option('--attack-mode', 'Enable attack mode (CAPTCHA protection)')
    .option('--no-attack-mode', 'Disable attack mode')
    .option('--preview-envs', 'Enable preview environments')
    .option('--no-preview-envs', 'Disable preview environments')
    .option(
      '--image-retention-hours <hours>',
      'Hours to keep built images before nightly cleanup removes them (1-8760). ' +
        'Images are needed to roll back, so this is the project rollback window'
    )
    .option(
      '--reset-image-retention',
      'Clear the per-project image retention override and use the system default'
    )
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts (for automation)')
    .action(updateSettingsAction)

  projects
    .command('git')
    .description('Update git repository settings')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--owner <owner>', 'Repository owner')
    .option('--repo <repo>', 'Repository name')
    .option('--branch <branch>', 'Main branch')
    .option('--directory <directory>', 'App directory path')
    .option('--preset <preset>', 'Build preset (auto, nextjs, nodejs, static, docker, rust, go, python)')
    .option('--connection <id>', 'Git connection ID (links the project to an actual clone-access connection; omit to leave the existing connection unchanged)')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts, use provided/existing values (for automation)')
    .action(updateGitAction)

  projects
    .command('config')
    .description('Update deployment configuration (resources, replicas)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--replicas <n>', 'Number of container replicas')
    .option('--cpu-limit <limit>', 'CPU limit in cores (e.g., 0.5, 1, 2)')
    .option('--memory-limit <limit>', 'Memory limit in MB')
    .option('--auto-deploy', 'Enable automatic deployments')
    .option('--no-auto-deploy', 'Disable automatic deployments')
    .option('--request-timeout <seconds>', 'Default timeout for regular HTTP requests, in seconds')
    .option('--sse-idle-timeout <seconds>', 'Default idle timeout for SSE streams, in seconds')
    .option('--websocket-idle-timeout <seconds>', 'Default idle timeout for WebSocket connections, in seconds')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts (for automation)')
    .action(updateConfigAction)

  projects
    .command('delete')
    .alias('rm')
    .description('Delete a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(remove)
}
