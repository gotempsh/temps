// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Visitor identity modes (ADR 109 D3), worded for the console.
 */

import type { AnalyticsIdentityMode } from '@/api/client/types.gen'

export interface IdentityModeOption {
  value: AnalyticsIdentityMode
  title: string
  summary: string
  consequences: string[]
}

export const IDENTITY_MODE_OPTIONS: IdentityModeOption[] = [
  {
    value: 'persistent',
    title: 'Recognise returning visitors',
    summary:
      'A first-party cookie (or localStorage when Temps does not serve the page) keeps a visitor id for a year.',
    consequences: [
      'Returning visitors, multi-day profiles, journeys and session replay work.',
      'In the EU this usually needs consent before tracking starts.',
    ],
  },
  {
    value: 'cookieless',
    title: 'Cookieless',
    summary:
      'Nothing is stored on the device. Visitors are counted by a hash of a daily salt, the address and the browser; the salt is deleted after a day.',
    consequences: [
      'A returning visitor counts as a new visitor the next day.',
      'Unique visitors over several days are the sum of daily uniques.',
      'Visitor profiles and journeys cover one day; session replay is off.',
    ],
  },
]

/** What the tracker on the site must be told so it stores nothing either. */
export function cookielessSnippets(): { react: string; script: string } {
  return {
    react: '<TempsAnalyticsProvider cookieless>\n  {children}\n</TempsAnalyticsProvider>',
    script:
      '<script defer src="https://cdn.jsdelivr.net/npm/@temps-sdk/analytics-browser/dist/temps.min.js"\n  data-cookieless="true"></script>',
  }
}

/** The fact line views that depend on returning visitors show in cookieless mode. */
export function cookielessFactLine(view: 'visitors' | 'journey' | 'replays'): string {
  switch (view) {
    case 'visitors':
      return 'Cookieless tracking: a visitor who returns on another day is listed as a new visitor.'
    case 'journey':
      return 'Cookieless tracking: a journey covers one day, since the visitor id changes daily.'
    case 'replays':
      return 'Cookieless tracking: session replay is off, because a replay needs an id stored on the device.'
  }
}
