// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type RightView =
  'generated' | 'files' | 'preview' | 'projects' | 'workspace'

export type ThreadListMode = 'active' | 'archived'

export type ApplicationListMode = 'active' | 'archived'

export type WorkspaceLoadPhase =
  'idle' | 'checking' | 'waking' | 'recovering' | 'inspecting'

export function mergeConversationPages<T extends { public_id: string }>(
  firstPage: T[],
  additionalPages: T[]
): T[] {
  const seen = new Set<string>()
  return [...firstPage, ...additionalPages].filter((conversation) => {
    if (seen.has(conversation.public_id)) return false
    seen.add(conversation.public_id)
    return true
  })
}
