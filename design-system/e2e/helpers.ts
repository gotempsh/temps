// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Page, TestInfo } from '@playwright/test'

/**
 * `?p=` view ids contain colons (`db:acme-pg`, `issue:i_4821`). Chromium
 * normalises a bare `:` in a query string, so encode it once, here, and let
 * every spec name views the way the docs do.
 */
export function v5(view: string, extra = ''): string {
  return `/v5?p=${view.replace(/:/g, '%3A')}${extra}`
}

/**
 * The sandbox themes through next-themes with `attribute="class"` and
 * `enableSystem={false}`, so the OS `colorScheme` emulation does nothing:
 * dark mode is the `theme` key in localStorage, written before the app boots.
 */
export async function setTheme(page: Page, theme: 'light' | 'dark'): Promise<void> {
  await page.addInitScript((t) => {
    try { window.localStorage.setItem('theme', t) } catch { /* first-party storage only */ }
  }, theme)
}

/** Wait for the app shell to be painted and for React to have settled. */
export async function ready(page: Page): Promise<void> {
  await page.waitForSelector('#root > *', { state: 'attached' })
  await page.waitForFunction(() => document.fonts.status === 'loaded').catch(() => {})
}

/**
 * Lines the sandbox's own tooling writes that are not a page defect: Vite's
 * HMR chatter, the React DevTools nag, and the source-map/preload notices a
 * dev server emits. Everything else is a real console error or warning.
 */
const BENIGN = [
  /\[vite\]/i,
  /\/@vite\//,
  /\/@react-refresh/,
  /\[HMR\]/i,
  /Download the React DevTools/i,
  /react[-_]devtools/i,
  /Failed to load resource: .*favicon/i,
  /Source ?-?map/i,
  /DevTools failed to load/i,
]

export type ConsoleNote = { type: string; text: string }

/**
 * Attach before navigating. Collects console errors/warnings and page errors,
 * minus the dev-server noise above.
 */
export function watchConsole(page: Page): ConsoleNote[] {
  const notes: ConsoleNote[] = []
  page.on('console', (msg) => {
    const type = msg.type()
    if (type !== 'error' && type !== 'warning') return
    const text = msg.text()
    if (BENIGN.some((re) => re.test(text))) return
    notes.push({ type, text })
  })
  page.on('pageerror', (err) => {
    const text = String(err?.message ?? err)
    if (BENIGN.some((re) => re.test(text))) return
    notes.push({ type: 'pageerror', text })
  })
  return notes
}

/** Format collected console notes for an assertion message. */
export function formatNotes(notes: ConsoleNote[]): string {
  return notes.map((n) => `[${n.type}] ${n.text}`).join('\n')
}

/**
 * The whole `?p=` route list the sandbox ships, in the order the handoff
 * lists them. Every one of these must lay out without a horizontal scrollbar
 * at 1440 and at 390.
 */
export const V5_VIEWS = [
  'projects',
  'api-gateway',
  'acme-storefront',
  'billing-worker',
  'databases',
  'db:acme-pg',
  'db:sessions-redis',
  'deploy:dep_91a',
  'deploy:dep_92e',
  'deploy:dep_92b',
  'errors',
  'issue:i_4821',
  'analytics',
  'event:signup',
  'uptime',
  'monitor:mon_2',
  'email',
  'domain:3',
  'proxy',
  'settings',
  'settings:nodes',
  'node:hetzner-3',
  'settings:cluster',
  'settings:builds',
  'settings:keys',
  'git',
  'git:1',
  'security',
  'sandboxes',
  'sandbox:sbx_7f21',
  'traces',
  'trace:3f9c1e7a8b2d4f60',
  'metrics',
  'backups',
] as const

/** Views that need an extra flag to reach the state under test. */
export const V5_VIEWS_WITH_FLAGS: ReadonlyArray<{ view: string; extra?: string; label: string }> = [
  ...V5_VIEWS.map((view) => ({ view, label: view })),
  { view: 'errors', extra: '&fail=1', label: 'errors&fail=1' },
  { view: 'analytics', extra: '&fresh=1', label: 'analytics&fresh=1' },
  { view: 'email', extra: '&fresh=1', label: 'email&fresh=1' },
]

/** Chrome-free routes outside the `?p=` console. */
export const STANDALONE_ROUTES: ReadonlyArray<{ path: string; label: string }> = [
  { path: '/status?project=acme-storefront', label: '/status?project=acme-storefront' },
  { path: '/guide', label: '/guide' },
  // The guide's longest section, and the one with the most rendered markdown:
  // wide tables and long code fences are where a document page overflows.
  { path: '/guide#taste', label: '/guide#taste' },
  { path: '/console', label: '/console' },
  { path: '/landing', label: '/landing' },
  { path: '/op-components', label: '/op-components' },
]

/** Report a moderate a11y violation as an annotation instead of a failure. */
export function annotate(testInfo: TestInfo, type: string, description: string): void {
  testInfo.annotations.push({ type, description })
}
