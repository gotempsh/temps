// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function workspacePageTitle(
  workspaceName?: string | null,
  threadTitle?: string | null
): string {
  const workspace = workspaceName?.trim() ?? ''
  const thread = threadTitle?.trim() ?? ''
  if (thread && workspace && thread !== workspace)
    return `${thread} · ${workspace}`
  return thread || workspace
}

export function threadTitleFromLiveEvent(
  eventName: string,
  data: string
): string | null {
  if (eventName !== 'conversation_title') return null
  try {
    const payload = JSON.parse(data) as { title?: unknown }
    return typeof payload.title === 'string' && payload.title.trim()
      ? payload.title
      : null
  } catch {
    return null
  }
}
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
