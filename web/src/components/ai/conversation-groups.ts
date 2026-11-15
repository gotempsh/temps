// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The part of a listed chat that decides where it goes in the list. */
export interface GroupableChat {
  public_id: string
  workspace?: { public_id: string; name: string } | null
}

/** One row of the chat list: a chat on its own, or a workspace with its chats. */
export type ChatListEntry<T extends GroupableChat> =
  | { kind: 'chat'; chat: T }
  | {
      kind: 'workspace'
      workspace: { public_id: string; name: string }
      chats: T[]
    }

/**
 * Gathers the chats of each workspace under one entry, kept where the
 * workspace's most recent chat would be, so the list still reads newest first
 * while a workspace's chats sit together instead of scattered among the rest.
 * The input is expected newest first; that order is kept within each group.
 */
export function groupChatsByWorkspace<T extends GroupableChat>(
  chats: T[]
): ChatListEntry<T>[] {
  const entries: ChatListEntry<T>[] = []
  const groups = new Map<string, T[]>()
  for (const chat of chats) {
    const workspace = chat.workspace
    if (!workspace) {
      entries.push({ kind: 'chat', chat })
      continue
    }
    const group = groups.get(workspace.public_id)
    if (group) {
      group.push(chat)
      continue
    }
    const created = [chat]
    groups.set(workspace.public_id, created)
    entries.push({ kind: 'workspace', workspace, chats: created })
  }
  return entries
}
