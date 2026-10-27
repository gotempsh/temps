// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { safeReturnTo } from '@/lib/safe-return-to'

const KEY = 'temps:returnTo'

/**
 * Paths that must never be captured as a post-login destination.
 *
 * `/login` matters most and is the least obvious: it is not a registered route
 * at all. It falls through to `/*` -> ProtectedLayout, which renders the login
 * form in place (the URL stays `/login`) and calls `captureReturnTo()`. Without
 * this entry, signing in at `/login` stored `/login` as the return target and
 * then navigated back to it -- now authenticated, where `AuthenticatedRoutes`
 * has no such route, so the user landed on "404 - Page Not Found" immediately
 * after a successful login. Bookmarking the login page is entirely normal, so
 * this was easy to hit and impossible to self-diagnose.
 *
 * The password-reset paths are here for the same reason: they are public routes
 * that make no sense to return to once a session exists.
 */
const AUTH_PATHS = new Set([
  '/mfa-verify',
  '/login',
  '/forgot-password',
  '/auth/reset-password',
])

function isAuthPath(path: string): boolean {
  const pathname = path.split('?')[0]?.split('#')[0] ?? path
  return AUTH_PATHS.has(pathname)
}

export function captureReturnTo(): void {
  if (typeof window === 'undefined') return
  const current = `${window.location.pathname}${window.location.search}${window.location.hash}`
  if (!current || current === '/' || isAuthPath(current)) return
  try {
    window.sessionStorage.setItem(KEY, current)
  } catch {
    /* storage disabled */
  }
}

/**
 * Remember `path` (rather than the current URL) as the post-login
 * destination. Used when a sign-in has to restart and the only record of
 * where the user was going is a `return_to` the server put in this page's
 * query string. Unsafe or auth-only paths are ignored.
 */
export function rememberReturnTo(path: string | null | undefined): void {
  if (typeof window === 'undefined') return
  const safe = safeReturnTo(path)
  if (!safe || safe === '/' || isAuthPath(safe)) return
  try {
    window.sessionStorage.setItem(KEY, safe)
  } catch {
    /* storage disabled */
  }
}

export function consumeReturnTo(fallback = '/dashboard'): string {
  if (typeof window === 'undefined') return fallback
  try {
    const value = window.sessionStorage.getItem(KEY)
    window.sessionStorage.removeItem(KEY)
    if (value && !isAuthPath(value)) return value
  } catch {
    /* storage disabled */
  }
  return fallback
}

/**
 * Paths the server answers rather than the console's router: the signed-in
 * gate's authorize step (`/_temps/gate/authorize`) sends a signed-out visitor
 * here to sign in and must be loaded as a page afterwards, not routed to.
 */
export function isServerPath(path: string): boolean {
  return path.startsWith('/_temps/')
}

/**
 * Go to a post-login destination: a full page load for a server path, the
 * router otherwise.
 */
export function goToReturnTo(
  navigate: (to: string, options?: { replace?: boolean }) => void,
  path: string
): void {
  if (isServerPath(path) && typeof window !== 'undefined') {
    window.location.replace(path)
    return
  }
  navigate(path, { replace: true })
}
