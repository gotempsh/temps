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
  logoBadge?: ReactNode
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
