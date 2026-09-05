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
