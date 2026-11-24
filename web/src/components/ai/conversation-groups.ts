// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The part of a listed chat that decides where it goes in the list. */
export interface GroupableChat {
  public_id: string
  title?: string | null
  project_name?: string | null
  workspace?: { public_id: string; name: string } | null
  /** ISO 8601 timestamp of the chat's latest message. */
  last_activity_at: string
}

export type ChatDateBucket = 'today' | 'yesterday' | 'week' | 'month' | 'older'

export const CHAT_DATE_BUCKET_LABELS: Record<ChatDateBucket, string> = {
  today: 'Today',
  yesterday: 'Yesterday',
  week: 'Previous 7 days',
  month: 'Previous 30 days',
  older: 'Older',
}

/** One heading of the chat list and the chats under it. */
export interface ChatDateGroup<T extends GroupableChat> {
  bucket: ChatDateBucket
  label: string
  chats: T[]
}

/**
 * Which heading a chat's last activity falls under, by the viewer's calendar
 * days rather than rolling 24-hour windows, so "Yesterday" means yesterday. An
 * unparseable timestamp files the chat under Older instead of dropping it.
 */
export function chatDateBucket(
  lastActivityAt: string,
  now: Date
): ChatDateBucket {
  const at = new Date(lastActivityAt)
  if (Number.isNaN(at.getTime())) return 'older'
  // Local midnight `daysAgo` calendar days back. Built from the date parts,
  // not by subtracting 24-hour periods, because a day that changes daylight
  // saving time lasts 23 or 25 hours.
  const startOfDay = (daysAgo: number) =>
    new Date(
      now.getFullYear(),
      now.getMonth(),
      now.getDate() - daysAgo
    ).getTime()
  const time = at.getTime()
  if (time >= startOfDay(0)) return 'today'
  if (time >= startOfDay(1)) return 'yesterday'
  if (time >= startOfDay(7)) return 'week'
  if (time >= startOfDay(30)) return 'month'
  return 'older'
}

/**
 * The chat list under date headings. The input is expected newest first; each
 * chat keeps its position within its heading, and headings appear in time
 * order. A chat with a timestamp in the future (clock skew) counts as today.
 */
export function groupChatsByDate<T extends GroupableChat>(
  chats: T[],
  now: Date = new Date()
): ChatDateGroup<T>[] {
  const order: ChatDateBucket[] = [
    'today',
    'yesterday',
    'week',
    'month',
    'older',
  ]
  const byBucket = new Map<ChatDateBucket, T[]>()
  for (const chat of chats) {
    const bucket = chatDateBucket(chat.last_activity_at, now)
    const group = byBucket.get(bucket)
    if (group) group.push(chat)
    else byBucket.set(bucket, [chat])
  }
  return order.flatMap((bucket) => {
    const grouped = byBucket.get(bucket)
    return grouped
      ? [{ bucket, label: CHAT_DATE_BUCKET_LABELS[bucket], chats: grouped }]
      : []
  })
}

/**
 * The loaded chats whose title, project or workspace contains every word of
 * `query`, ignoring case. An empty query keeps every chat. This only filters
 * what is loaded; the switcher (⌘K) searches the server for older chats.
 */
export function filterChats<T extends GroupableChat>(
  chats: T[],
  query: string
): T[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean)
  if (words.length === 0) return chats
  return chats.filter((chat) => {
    const haystack = [chat.title, chat.project_name, chat.workspace?.name]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
    return words.every((word) => haystack.includes(word))
  })
}
