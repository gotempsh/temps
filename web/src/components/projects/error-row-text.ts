// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What an error group's row says, each part once.
 *
 * SDKs title a group "<type>: <message>" and also send the message on its
 * own, so rendering the type badge, the title and the message verbatim
 * repeated the type twice and the message twice. The badge carries the type;
 * the headline is the title without that prefix; the detail line appears only
 * when the message adds something the headline does not already say.
 */
export interface ErrorRowText {
  /** The exception type for the badge, as the SDK reported it. */
  type: string
  headline: string
  /** A second line, or null when it would repeat the headline. */
  detail: string | null
}

export function errorRowText(group: {
  error_type?: string | null
  title: string
  message_template?: string | null
}): ErrorRowText {
  const type = group.error_type?.trim() || 'error'
  const title = group.title.trim()
  const prefix = `${type}: `
  const headline =
    title.startsWith(prefix) && title.length > prefix.length
      ? title.slice(prefix.length)
      : title
  const message = group.message_template?.trim() ?? ''
  const detail =
    message && message !== headline && !title.endsWith(message) ? message : null
  return { type, headline, detail }
}
