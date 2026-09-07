// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
