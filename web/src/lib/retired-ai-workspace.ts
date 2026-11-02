// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const SAFE_ID = /^[a-zA-Z0-9_-]+$/

/**
 * Where an old `/ai-first` link lands now that the AI workspace page is
 * retired: the conversation it had open (`?thread=`, which the chat page can
 * open by id), else the workspace it named (`?application=` or
 * `?scope=global`), else the workspaces list. Setup parameters had no other
 * home, so they are dropped rather than carried somewhere that ignores them.
 */
export function retiredAiWorkspaceTarget(search: string): string {
  const params = new URLSearchParams(search)
  const thread = params.get('thread')
  if (thread && SAFE_ID.test(thread)) return `/chat?chat=${thread}`
  if (params.get('scope') === 'global') return '/workspaces/global'
  const application = params.get('application')
  if (application && SAFE_ID.test(application))
    return `/workspaces/${application}`
  return '/workspaces'
}
