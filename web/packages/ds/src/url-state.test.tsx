// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { createSearchParamsBatch, useUrlState } from './url-state'

/**
 * Models what React Router does: `setSearchParams(fn)` calls `fn` with the
 * params of the render the callback belongs to, and the new location only
 * becomes visible to callers after the next render.
 */
function fakeRouter(initial: string) {
  let location = new URLSearchParams(initial)
  let rendered = new URLSearchParams(initial)
  return {
    get location() {
      return location.toString()
    },
    render() {
      rendered = new URLSearchParams(location)
      return rendered
    },
    get rendered() {
      return rendered
    },
    navigate(next: URLSearchParams) {
      location = new URLSearchParams(next)
    },
    renderTimeUpdate(fn: (current: URLSearchParams) => URLSearchParams) {
      location = fn(new URLSearchParams(rendered))
    },
  }
}

const withKeys =
  (values: Record<string, string | undefined>) =>
  (current: URLSearchParams) => {
    const next = new URLSearchParams(current)
    for (const [key, value] of Object.entries(values)) {
      if (value === undefined) next.delete(key)
      else next.set(key, value)
    }
    return next
  }

describe('search params batching', () => {
  test('two updates in one tick both land', () => {
    const router = fakeRouter('page=3&tab=a')
    const batch = createSearchParamsBatch()
    router.navigate(batch.apply(router.rendered, withKeys({ q: 'qa' })))
    router.navigate(batch.apply(router.rendered, withKeys({ page: undefined })))
    const result = new URLSearchParams(router.location)
    expect(result.get('q')).toBe('qa')
    expect(result.has('page')).toBe(false)
    expect(result.get('tab')).toBe('a')
  })

  test('a plain value update is also chained, not just functional ones', () => {
    const router = fakeRouter('a=1')
    const batch = createSearchParamsBatch()
    router.navigate(batch.apply(router.rendered, withKeys({ b: '2' })))
    router.navigate(batch.apply(router.rendered, new URLSearchParams('c=3')))
    expect(router.location).toBe('c=3')
    router.navigate(batch.apply(router.rendered, withKeys({ d: '4' })))
    expect(router.location).toBe('c=3&d=4')
  })

  test('keeps chaining while the router renders an intermediate step', () => {
    const router = fakeRouter('')
    const batch = createSearchParamsBatch()
    router.navigate(batch.apply(router.rendered, withKeys({ a: '1' })))
    router.navigate(batch.apply(router.rendered, withKeys({ b: '2' })))
    // Router renders the first step only.
    const intermediate = new URLSearchParams('a=1')
    batch.observe(intermediate)
    router.navigate(batch.apply(intermediate, withKeys({ c: '3' })))
    expect(router.location).toBe('a=1&b=2&c=3')
  })

  test('starts from the rendered location once the chain has landed', () => {
    const router = fakeRouter('')
    const batch = createSearchParamsBatch()
    router.navigate(batch.apply(router.rendered, withKeys({ a: '1' })))
    batch.observe(router.render())
    // Someone else (back button) changed the URL after that.
    const external = new URLSearchParams('z=9')
    batch.observe(external)
    expect(batch.apply(external, withKeys({ b: '2' })).toString()).toBe(
      'z=9&b=2'
    )
  })

  test('an external navigation ends the chain', () => {
    const batch = createSearchParamsBatch()
    batch.apply(new URLSearchParams('a=1'), withKeys({ q: 'x' }))
    const external = new URLSearchParams('other=1')
    batch.observe(external)
    expect(batch.apply(external, withKeys({ page: '2' })).toString()).toBe(
      'other=1&page=2'
    )
  })

  test('a chain that never landed expires', () => {
    let now = 1_000
    const batch = createSearchParamsBatch({ maxAgeMs: 500, now: () => now })
    const rendered = new URLSearchParams('a=1')
    batch.apply(rendered, withKeys({ q: 'x' }))
    now += 501
    expect(batch.apply(rendered, withKeys({ page: '2' })).toString()).toBe(
      'a=1&page=2'
    )
  })

  test('does not mutate the params it is given', () => {
    const batch = createSearchParamsBatch()
    const rendered = new URLSearchParams('a=1')
    batch.apply(rendered, withKeys({ q: 'x' }))
    expect(rendered.toString()).toBe('a=1')
  })
})

describe('useUrlState', () => {
  test('reads typed state from the location', () => {
    let seen: Record<string, string | undefined> = {}
    function Probe() {
      const { state, get } = useUrlState<'q' | 'page'>()
      seen = { q: get('q') ?? undefined, page: state.page }
      return null
    }
    renderToStaticMarkup(
      <MemoryRouter initialEntries={['/backups?q=nightly&page=2']}>
        <Probe />
      </MemoryRouter>
    )
    expect(seen).toEqual({ q: 'nightly', page: '2' })
  })
})
