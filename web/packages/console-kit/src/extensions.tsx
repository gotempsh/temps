// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactElement, ReactNode } from 'react'

export interface ConsoleNavItem {
  id: string
  label: string
  path: string
  icon?: ReactNode
  section?: string
}

export interface ConsoleRoute {
  path: string
  element: ReactElement
}

/**
 * A minimal project identity, passed to `ConsoleProjectToolLink.href` so an
 * extension can build a link scoped to whichever project the sidebar is
 * currently showing.
 */
export interface ConsoleProjectContext {
  id: number
  slug: string
}

/**
 * A link in the project detail sidebar that navigates OUTSIDE the project's
 * own route tree (`/projects/:slug/...`) — unlike `projectToolGroups`'s
 * items, which are relative paths nested under it.
 *
 * Exists for extension-provided features that manage their own top-level
 * page with an internal project picker (the established EE pattern — see
 * e.g. Retention, ApprovalGates, OTel Forwarding) rather than living inside
 * the project's route tree. Without this, those features are reachable only
 * from the global Enterprise nav section, and a user already looking at a
 * specific project has no path to "this feature, for this project" — they
 * have to leave, find the global entry, then reselect the project they were
 * just looking at.
 */
export interface ConsoleProjectToolLink {
  /** Stable id (React key). */
  id: string
  title: string
  icon?: ReactNode
  /** Built from the current project so the link opens pre-scoped to it. */
  href: (project: ConsoleProjectContext) => string
}

/**
 * An action rendered in the top-right of the console header, left of the
 * built-in Create / alerts / theme controls. Intended for compact icon
 * buttons that navigate to or open an extension surface (e.g. EE's SRE
 * Copilot). Order follows array order.
 */
export interface ConsoleHeaderAction {
  /** Stable id (React key + test hook). */
  id: string
  /** The rendered control — typically an icon `Button`. The extension owns
   *  its own onClick/navigation; the console shell only places it. */
  element: ReactNode
}

export interface ConsoleExtensions {
  routes?: ConsoleRoute[]
  navItems?: ConsoleNavItem[]
  /** Compact actions placed top-right in the header (see [`ConsoleHeaderAction`]). */
  headerActions?: ConsoleHeaderAction[]
  /**
   * Replaces the literal "Temps" wordmark in the sidebar header. Falls
   * back to "Temps" when unset.
   *
   * `ReactNode`, not `string` — a product name is typically fetched
   * asynchronously (e.g. from a branding settings endpoint), so the
   * extension supplies a small component that resolves its own data and
   * falls back to "Temps" itself while loading, the same pattern
   * `headerActions` and `loginPage` already use for slots backed by a query.
   *
   * Exists for white-label branding: an operator-configured product name
   * has nowhere else to appear in the primary navigation chrome, and
   * `logoBadge` (a small pill *beside* the wordmark) is the wrong shape
   * for a name meant to replace it rather than annotate it.
   */
  logoText?: ReactNode
  /**
   * Replaces the fixed `/svg/temps-icon.svg` mark in the sidebar header.
   * Falls back to the default icon when unset. Same `ReactNode`-not-`string`
   * reasoning as `logoText` — the icon is typically an async-fetched URL, so
   * the extension supplies a component that resolves its own data and falls
   * back to the default icon itself (including on a broken/unreachable
   * image URL) rather than this slot trying to validate it centrally.
   */
  logoIcon?: ReactNode
  logoBadge?: ReactNode
  /** Extra links in the project detail sidebar. See `ConsoleProjectToolLink`. */
  projectToolLinks?: ConsoleProjectToolLink[]
  /**
   * Replace the OSS unauthenticated login screen with an extension-provided
   * element. When set, `ProtectedLayout` renders this instead of the
   * built-in `<Login />` for any unauthenticated request.
   *
   * The element is responsible for rendering the entire screen (logo,
   * card, form). It also needs to navigate the user somewhere after a
   * successful sign-in — typically by reading `returnTo` from the URL
   * the way the OSS `<Login />` does, or by relying on the page that
   * gated them to re-render on session change.
   *
   * Today's only consumer: temps-ee's password-login policy. When the
   * EE operator disables password login, the EE Login swaps the
   * email/password form for an SSO-only view.
   *
   * Keep this as a single slot rather than a generic `overrides` map:
   * if/when EE needs to swap a second component, add a sibling slot
   * (e.g. `mfaPage`, `errorPage`) explicitly. Discoverability over
   * cleverness — the next reader sees exactly which screens can be
   * replaced.
   */
  loginPage?: ReactElement
}

export const emptyConsoleExtensions: ConsoleExtensions = {}
