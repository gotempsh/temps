import type { Command } from 'commander'
import { list } from './list.js'
import { create } from './create.js'
import { show } from './show.js'
import { remove } from './delete.js'
import { updateProjectAction, updateSettingsAction, updateGitAction, updateConfigAction } from './update.js'

export function registerProjectsCommands(program: Command): void {
  const projects = program
    .command('projects')
    .alias('project')
    .alias('p')
    .description('Manage projects')

  projects
    .command('list')
    .alias('ls')
    .description('List all projects')
    .option('--json', 'Output in JSON format')
    .action(list)

  projects
    .command('create')
    .alias('new')
    .description('Create a new project')
    .option('-n, --name <name>', 'Project name')
    .option('-d, --description <description>', 'Project description')
    .option('--repo <repository>', 'Git repository URL')
    .action(create)

  projects
    .command('show')
    .alias('get')
    .description('Show project details')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(show)

  projects
    .command('update')
    .alias('edit')
    .description('Update project name and description')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('-n, --name <name>', 'New project name')
    .option('-d, --description <description>', 'New project description')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts, use provided values (for automation)')
    .action(updateProjectAction)

  projects
    .command('settings')
    .description('Update project settings (slug, attack mode, preview environments)')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('--slug <slug>', 'Project URL slug')
    .option('--attack-mode', 'Enable attack mode (CAPTCHA protection)')
    .option('--no-attack-mode', 'Disable attack mode')
    .option('--preview-envs', 'Enable preview environments')
    .option('--no-preview-envs', 'Disable preview environments')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts (for automation)')
    .action(updateSettingsAction)

  projects
    .command('git')
    .description('Update git repository settings')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('--owner <owner>', 'Repository owner')
    .option('--repo <repo>', 'Repository name')
    .option('--branch <branch>', 'Main branch')
    .option('--directory <directory>', 'App directory path')
    .option('--preset <preset>', 'Build preset (auto, nextjs, nodejs, static, docker, rust, go, python)')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts, use provided/existing values (for automation)')
    .action(updateGitAction)

  projects
    .command('config')
    .description('Update deployment configuration (resources, replicas)')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('--replicas <n>', 'Number of container replicas')
    .option('--cpu-limit <limit>', 'CPU limit in cores (e.g., 0.5, 1, 2)')
    .option('--memory-limit <limit>', 'Memory limit in MB')
    .option('--auto-deploy', 'Enable automatic deployments')
    .option('--no-auto-deploy', 'Disable automatic deployments')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip prompts (for automation)')
    .action(updateConfigAction)

  projects
    .command('delete')
    .alias('rm')
    .description('Delete a project')
    .requiredOption('-p, --project <project>', 'Project slug or ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(remove)
}
