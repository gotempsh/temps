import type { Command } from 'commander'
import { list } from './list.js'
import { create } from './create.js'
import { show } from './show.js'
import { remove } from './delete.js'

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
    .command('show <project>')
    .alias('get')
    .description('Show project details')
    .option('--json', 'Output in JSON format')
    .action(show)

  projects
    .command('delete <project>')
    .alias('rm')
    .description('Delete a project')
    .option('-f, --force', 'Skip confirmation')
    .action(remove)
}
