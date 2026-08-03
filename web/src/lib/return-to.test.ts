import { beforeEach, describe, expect, test } from 'bun:test'

/**
 * Regression coverage for the post-login 404.
 *
 * `/login` is not a registered route: it falls through to `/*` ->
 * ProtectedLayout, which renders the login form in place (leaving the URL as
 * `/login`) and calls `captureReturnTo()`. Before the fix that stored `/login`
 * as the return target, so a *successful* sign-in navigated back to `/login`
 * -- now authenticated, where `AuthenticatedRoutes` has no such route -- and
 * the user landed on "404 - Page Not Found".
 *
 * `bun test` runs without a DOM and return-to.ts only reaches for
 * `window.location` and `window.sessionStorage`, so a small stub is cheaper and
 * clearer here than pulling in a full DOM implementation. It must be installed
 * before the module under test is imported, because the module is evaluated at
 * import time.
 */
const store = new Map<string, string>()

Object.defineProperty(globalThis, 'window', {
  configurable: true,
  value: {
    location: { pathname: '/', search: '', hash: '' },
    sessionStorage: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
      removeItem: (key: string) => void store.delete(key),
      clear: () => store.clear(),
    },
  },
})

const { captureReturnTo, consumeReturnTo } = await import('./return-to')

function setLocation(path: string) {
  const [pathAndSearch = '', hash = ''] = path.split('#')
  const [pathname = '', search = ''] = pathAndSearch.split('?')
  window.location.pathname = pathname
  window.location.search = search ? `?${search}` : ''
  window.location.hash = hash ? `#${hash}` : ''
}

describe('captureReturnTo', () => {
  beforeEach(() => {
    store.clear()
  })

  test('remembers a real destination so the user is returned to it', () => {
    setLocation('/storage?tab=external')
    captureReturnTo()

    expect(consumeReturnTo()).toBe('/storage?tab=external')
  })

  // /login is the one that caused the reported 404; the rest are auth-only
  // routes that make no sense to return to once a session exists.
  test.each([
    '/login',
    '/mfa-verify',
    '/forgot-password',
    '/auth/reset-password',
  ])('never captures %s as a post-login destination', (path) => {
    setLocation(path)
    captureReturnTo()

    expect(consumeReturnTo('/dashboard')).toBe('/dashboard')
  })

  test('ignores an auth path that carries a query string', () => {
    setLocation('/login?next=%2Fstorage')
    captureReturnTo()

    expect(consumeReturnTo('/dashboard')).toBe('/dashboard')
  })

  test('ignores the root path, which is the default destination anyway', () => {
    setLocation('/')
    captureReturnTo()

    expect(consumeReturnTo('/dashboard')).toBe('/dashboard')
  })

  test('consuming clears the stored value so it is not reused later', () => {
    setLocation('/domains')
    captureReturnTo()

    expect(consumeReturnTo('/dashboard')).toBe('/domains')
    expect(consumeReturnTo('/dashboard')).toBe('/dashboard')
  })
})
