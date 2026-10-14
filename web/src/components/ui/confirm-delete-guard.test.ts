// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

/**
 * Every module that can fire a destructive API call from the console must
 * route it through `ConfirmDeleteDialog`, which names the thing being removed.
 *
 * "Can fire a destructive call" is detected from the imports: a name from the
 * generated client (`@/api/client`) that starts with delete/remove/revoke/
 * destroy/purge/unlink/detach/disconnect/clear/uninstall/terminate, whether the SDK function itself
 * (`deleteFoo`) or its react-query options (`deleteFooMutation`). A module
 * that imports one must also import `ConfirmDeleteDialog`.
 *
 * Add a module to `EXEMPT` only when it genuinely cannot be reached by a click
 * without a confirmation owned elsewhere (a helper whose callers own the
 * dialog) or the action is not destructive for the user. The reason is
 * mandatory, and a stale entry fails this test.
 */
const DESTRUCTIVE_NAME =
  /^(delete|remove|revoke|destroy|purge|unlink|detach|disconnect|clear|uninstall|terminate)[A-Z]/
const DIALOG_IMPORT =
  /import\s*\{[^}]*\bConfirmDeleteDialog\b[^}]*\}\s*from\s*['"]@\/components\/ui\/confirm-delete-dialog['"]/

const EXEMPT: Record<string, string> = {
  'src/components/email/sharedUtils.ts':
    'Exports a thin deleteEmailProvider helper; its callers (EmailProvidersManagement, EmailProviderDetail) own the ConfirmDeleteDialog.',
  'src/lib/backup-cleanup.ts':
    'Exports a thin deleteBackup helper; its only caller (BackupDetail) owns the ConfirmDeleteDialog.',
  'src/hooks/usePlugins.ts':
    'Hook module exporting useUninstallPlugin; its only caller (PluginsPage) owns the ConfirmDeleteDialog.',
  'src/pages/Drop.tsx':
    'Calls deleteProject only to roll back the service this same Drop flow just created after a failed upload; automatic cleanup, never user-triggered.',
}

const root = new URL('../../../', import.meta.url).pathname

/** Names imported from the generated client in `source`, aliases resolved to the exported name. */
export function clientImports(source: string): string[] {
  const names: string[] = []
  const importRe =
    /import\s*(?:type\s*)?\{([^}]*)\}\s*from\s*['"]@\/api\/client[^'"]*['"]/g
  for (const match of source.matchAll(importRe)) {
    for (const part of match[1].split(',')) {
      const spec = part.trim()
      if (!spec || spec.startsWith('type ')) continue
      names.push(spec.split(/\s+as\s+/)[0].trim())
    }
  }
  return names
}

export function destructiveImports(source: string): string[] {
  return clientImports(source).filter((n) => DESTRUCTIVE_NAME.test(n))
}

async function sourceFiles(): Promise<string[]> {
  const files: string[] = []
  for await (const path of new Bun.Glob('src/**/*.{ts,tsx}').scan(root)) {
    if (path.startsWith('src/api/client/')) continue
    if (path.startsWith('src/test/')) continue
    if (/\.test\.tsx?$/.test(path)) continue
    files.push(path)
  }
  return files.sort()
}

test('modules that import a destructive API call also import ConfirmDeleteDialog', async () => {
  const violations: string[] = []
  for (const path of await sourceFiles()) {
    if (path in EXEMPT) continue
    const source = await Bun.file(`${root}${path}`).text()
    const destructive = destructiveImports(source)
    if (destructive.length === 0) continue
    if (!DIALOG_IMPORT.test(source)) {
      violations.push(`${path}: ${destructive.join(', ')}`)
    }
  }
  expect(violations).toEqual([])
})

test('every exemption is justified and still needed', async () => {
  for (const [path, reason] of Object.entries(EXEMPT)) {
    expect(reason.trim().length).toBeGreaterThan(20)
    const file = Bun.file(`${root}${path}`)
    expect(await file.exists()).toBe(true)
    const source = await file.text()
    expect(destructiveImports(source).length).toBeGreaterThan(0)
    expect(DIALOG_IMPORT.test(source)).toBe(false)
  }
})

test('the detector recognises SDK functions, mutation options and aliases', () => {
  const source = `
    import {
      deleteEmailProvider as deleteApi,
      listEmailProviders,
      removeTeamMemberMutation,
      type DeleteThing,
    } from '@/api/client'
    import { revokeApiKeyMutation } from '@/api/client/@tanstack/react-query.gen'
  `
  expect(destructiveImports(source)).toEqual([
    'deleteEmailProvider',
    'removeTeamMemberMutation',
    'revokeApiKeyMutation',
  ])
})
