import type { Command } from 'commander'
import { listPluginsAction } from './list.js'
import { installPluginAction } from './install.js'
import { pluginCatalogAction } from './catalog.js'

export function registerPluginsCommands(program: Command): void {
  const plugins = program
    .command('plugins')
    .description('Discover and install external Temps plugins (e.g. VibeTemps)')

  plugins
    .command('list')
    .alias('ls')
    .description('List plugins available for install and whether they are already installed')
    .option('--json', 'Output in JSON format')
    .action(listPluginsAction)

  plugins
    .command('catalog')
    .description('Browse every plugin published in the Temps registry, including ones this instance is too old to install')
    .option('--json', 'Output in JSON format')
    .action(pluginCatalogAction)

  plugins
    .command('install <name>')
    .description('Download, verify, and install an external plugin binary')
    .option('--version <version>', 'Specific version hint (currently unused server-side; install always fetches latest)')
    .option('--json', 'Output in JSON format')
    .action(installPluginAction)
}
