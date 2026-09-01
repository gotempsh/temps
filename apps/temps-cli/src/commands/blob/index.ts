// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient } from '../../lib/api-client.js'
import { newline, header, icons, info, colors } from '../../ui/output.js'

export function registerBlobCommands(program: Command): void {
  const blob = program
    .command('blob')
    .description('Blob storage commands (coming soon)')

  blob
    .command('list')
    .alias('ls')
    .description('List blobs in a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--prefix <prefix>', 'Filter by key prefix')
    .option('--json', 'Output in JSON format')
    .action(comingSoonAction)

  blob
    .command('upload')
    .alias('put')
    .description('Upload a file as a blob')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Blob key (path)')
    .requiredOption('--file <path>', 'Local file path to upload')
    .action(comingSoonAction)

  blob
    .command('delete')
    .alias('rm')
    .description('Delete a blob')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Blob key to delete')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(comingSoonAction)

  blob
    .command('copy')
    .alias('cp')
    .description('Copy a blob to a new key')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--source <key>', 'Source blob key')
    .requiredOption('--dest <key>', 'Destination blob key')
    .action(comingSoonAction)

  blob
    .command('download')
    .alias('get')
    .description('Download a blob to a local file')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Blob key to download')
    .requiredOption('--output <path>', 'Local file path to save to')
    .action(comingSoonAction)

  blob
    .command('head')
    .description('Get blob metadata (size, content type, etc.)')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Blob key')
    .option('--json', 'Output in JSON format')
    .action(comingSoonAction)

  blob
    .command('enable')
    .description('Enable blob storage for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .action(comingSoonAction)

  blob
    .command('disable')
    .description('Disable blob storage for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .action(comingSoonAction)

  blob
    .command('status')
    .description('Get blob storage status for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(comingSoonAction)
}

async function comingSoonAction(): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()
  header(`${icons.info} Blob Storage`)
  newline()
  info(`${colors.bold('Blob storage commands are coming soon.')}`)
  newline()
  info('The Blob storage API is currently under development.')
  info('Check back in a future release for full blob storage support.')
  newline()
  info('Planned features:')
  console.log(`  ${colors.muted('-')} Upload and download files`)
  console.log(`  ${colors.muted('-')} List blobs with prefix filtering`)
  console.log(`  ${colors.muted('-')} Copy blobs between keys`)
  console.log(`  ${colors.muted('-')} Blob metadata inspection`)
  console.log(`  ${colors.muted('-')} Per-project blob store provisioning`)
  newline()
}
