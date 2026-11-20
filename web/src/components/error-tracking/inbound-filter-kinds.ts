// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Inbound filter kinds, as the API names them. */
export type InboundFilterKindName =
  | 'browser_extensions'
  | 'web_crawlers'
  | 'localhost'
  | 'error_message'
  | 'release'

export interface InboundFilterKindInfo {
  kind: InboundFilterKindName
  label: string
  description: string
  /** Pattern kinds take a glob and may be added more than once. */
  takesPattern: boolean
  placeholder?: string
}

export const INBOUND_FILTER_KINDS: InboundFilterKindInfo[] = [
  {
    kind: 'browser_extensions',
    label: 'Browser extensions',
    description:
      'Errors thrown inside a browser extension (a stack frame from chrome-extension://, moz-extension://, safari-extension://).',
    takesPattern: false,
  },
  {
    kind: 'web_crawlers',
    label: 'Web crawlers',
    description:
      'Errors from requests whose user agent is a known crawler or bot (Googlebot, Bingbot, GPTBot, uptime checkers).',
    takesPattern: false,
  },
  {
    kind: 'localhost',
    label: 'Localhost',
    description:
      'Errors from pages served on localhost or 127.0.0.1, usually a developer machine.',
    takesPattern: false,
  },
  {
    kind: 'error_message',
    label: 'Error message',
    description:
      'Errors whose "Type: message" matches a pattern. * matches anything, ? one character; case does not matter.',
    takesPattern: true,
    placeholder: '*ResizeObserver loop*',
  },
  {
    kind: 'release',
    label: 'Release',
    description:
      'Errors from releases matching a pattern, such as an old version still running somewhere.',
    takesPattern: true,
    placeholder: '1.2.*',
  },
]

export function inboundFilterKindInfo(
  kind: string
): InboundFilterKindInfo | undefined {
  return INBOUND_FILTER_KINDS.find((info) => info.kind === kind)
}

/**
 * The kinds that can still be added: every pattern kind, and the built-in
 * kinds the project does not have yet.
 */
export function addableInboundFilterKinds(
  existing: { kind: string }[]
): InboundFilterKindInfo[] {
  const present = new Set(existing.map((filter) => filter.kind))
  return INBOUND_FILTER_KINDS.filter(
    (info) => info.takesPattern || !present.has(info.kind)
  )
}
