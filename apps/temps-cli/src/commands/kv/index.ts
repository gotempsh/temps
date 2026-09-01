// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient } from '../../lib/api-client.js'
import { newline, header, icons, info, colors } from '../../ui/output.js'

export function registerKvCommands(program: Command): void {
  const kv = program
    .command('kv')
    .description('KV store commands (coming soon)')

  kv
    .command('get')
    .description('Get a value by key')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Key to retrieve')
    .action(comingSoonAction)

  kv
    .command('set')
    .description('Set a key-value pair')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Key to set')
    .requiredOption('--value <value>', 'Value to set')
    .option('--ttl <seconds>', 'Time-to-live in seconds')
    .action(comingSoonAction)

  kv
    .command('del')
    .alias('delete')
    .description('Delete a key')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Key to delete')
    .action(comingSoonAction)

  kv
    .command('keys')
    .alias('ls')
    .description('List keys')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--pattern <pattern>', 'Key pattern to filter by (e.g., "user:*")')
    .option('--json', 'Output in JSON format')
    .action(comingSoonAction)

  kv
    .command('ttl')
    .description('Get the TTL (time-to-live) for a key')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Key to check')
    .action(comingSoonAction)

  kv
    .command('expire')
    .description('Set expiry on an existing key')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Key to set expiry on')
    .requiredOption('--ttl <seconds>', 'Time-to-live in seconds')
    .action(comingSoonAction)

  kv
    .command('incr')
    .description('Increment a numeric value')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--key <key>', 'Key to increment')
    .action(comingSoonAction)

  kv
    .command('enable')
    .description('Enable KV store for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .action(comingSoonAction)

  kv
    .command('disable')
    .description('Disable KV store for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .action(comingSoonAction)

  kv
    .command('status')
    .description('Get KV store status for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(comingSoonAction)
}

async function comingSoonAction(): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()
  header(`${icons.info} KV Store`)
  newline()
  info(`${colors.bold('KV store commands are coming soon.')}`)
  newline()
  info('The KV store API is currently under development.')
  info('Check back in a future release for full KV store support.')
  newline()
  info('Planned features:')
  console.log(`  ${colors.muted('-')} Get, set, and delete key-value pairs`)
  console.log(`  ${colors.muted('-')} Key listing with pattern matching`)
  console.log(`  ${colors.muted('-')} TTL management and expiration`)
  console.log(`  ${colors.muted('-')} Atomic increment operations`)
  console.log(`  ${colors.muted('-')} Per-project KV store provisioning`)
  newline()
}
