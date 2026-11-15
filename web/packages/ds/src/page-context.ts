// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

/**
 * True inside a `PageContainer`. A page template (`Ledger`, `CardGrid`,
 * `Detail`) rendered by a route shell that already owns the page column would
 * otherwise add its gutters a second time and sit indented from the header.
 */
export const InsidePageContainerContext = createContext(false)

/**
 * True when the chrome around the page already names it, e.g. a section's
 * tab strip with this page's tab selected. The page's own `h1` then stays for
 * assistive technology but is not shown a second time.
 */
export const PageNamedByChromeContext = createContext(false)

/** Whether the surrounding chrome already shows this page's title. */
export function usePageNamedByChrome(): boolean {
  return useContext(PageNamedByChromeContext)
}

/**
 * The heading level of a page's title. `h1` unless the frame around the page
 * already has the top heading: a project's bar names the project, so the
 * pages inside it title themselves one level down.
 */
export const PageTitleLevelContext = createContext<'h1' | 'h2'>('h1')

/** The element a page title renders as: `h1`, or `h2` inside a frame. */
export function usePageTitleLevel(): 'h1' | 'h2' {
  return useContext(PageTitleLevelContext)
}
