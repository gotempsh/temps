// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where an old `/ai-first` link lands now that the AI workspace page is
 * retired: the workspace it named (`?application=` or `?scope=global`), or
 * the workspaces list. Thread and setup parameters had no other home, so
 * they are dropped rather than carried somewhere that ignores them.
 */
export function retiredAiWorkspaceTarget(search: string): string {
  const params = new URLSearchParams(search)
  if (params.get('scope') === 'global') return '/workspaces/global'
  const application = params.get('application')
  if (application && /^[a-zA-Z0-9_-]+$/.test(application))
    return `/workspaces/${application}`
  return '/workspaces'
}
