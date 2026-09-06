// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link, useLocation } from 'react-router'
import { useTheme } from 'next-themes'
import { Moon, Sun } from 'lucide-react'
import { Kbd } from '@/components/op'
import { LogoMark } from '@/components/Logo'
import { ShellContext, type Shell, type TocEntry } from '@/components/shell-context'
import { GUIDE_NAV, sectionFromHash } from '@/sections/Guide'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   The one chrome. Every page of the sandbox that is documentation — the
   guide and the ten reference pages — renders inside it, so moving between
   them moves nothing: same top bar, same rails, same column.

   Top bar   mark · name · what this is · filter · light/dark
   Left      the 15 guide sections, then the reference pages
   Middle    the page, starting at the column's left edge
   Right     "on this page", fed by whatever is in the middle

   Only three routes stay outside it — `/console`, `/landing`, `/status` —
   because they are the product with no sandbox around it.
   ──────────────────────────────────────────────────────────────────────── */

type RefEntry = { to: string; label: string }

/** The reference pages, in the order the README table lists them. */
const REFERENCE: readonly RefEntry[] = [
  { to: '/brand', label: 'Brand' },
  { to: '/foundations', label: 'Foundations' },
  { to: '/components', label: 'Components' },
  { to: '/op-components', label: 'Operator components' },
  { to: '/patterns', label: 'Page patterns' },
  { to: '/kitchen-sink', label: 'Kitchen sink' },
  { to: '/v1', label: 'Operator console v1' },
  { to: '/v1-landing', label: 'Landing v1' },
  { to: '/status-page', label: 'Status page' },
  { to: '/agent', label: 'Agent conversation' },
]

/**
 * Routes where the page, not the shell, owns `/` (and every other key): they
 * render a whole console or landing page, and its keyboard model is the point.
 */
const PAGE_OWNS_SLASH = new Set(['/v1', '/v1-landing', '/status-page', '/agent', '/kitchen-sink'])

function ThemeToggle() {
  const { resolvedTheme, setTheme } = useTheme()
  const isDark = resolvedTheme === 'dark'
  return (
    <button
      type="button"
      onClick={() => setTheme(isDark ? 'light' : 'dark')}
      className="flex h-7 w-7 shrink-0 items-center justify-center border hover:bg-muted"
    >
      {isDark ? <Sun className="h-3.5 w-3.5" /> : <Moon className="h-3.5 w-3.5" />}
      <span className="sr-only">Switch to {isDark ? 'light' : 'dark'} mode</span>
    </button>
  )
}

/**
 * The hash, tracked by hand. Guide section links are ordinary in-document
 * anchors — they fire `hashchange` and never `popstate`, so react-router's
 * own location goes stale on exactly the navigation the rail has to mark.
 */
function useHash(): string {
  const location = useLocation()
  const [hash, setHash] = useState(() => (typeof window === 'undefined' ? '' : window.location.hash))
  useEffect(() => setHash(window.location.hash), [location])
  useEffect(() => {
    const apply = () => setHash(window.location.hash)
    window.addEventListener('hashchange', apply)
    return () => window.removeEventListener('hashchange', apply)
  }, [])
  return hash
}

export function Layout({ children }: { children: ReactNode }) {
  const [query, setQuery] = useState('')
  const [toc, setToc] = useState<readonly TocEntry[]>([])
  const filterRef = useRef<HTMLInputElement>(null)
  const location = useLocation()
  const hash = useHash()

  const onGuide = location.pathname === '/guide'
  // On the guide the rail marks the section being read; anywhere else the
  // page itself is the current entry.
  const currentGuide = onGuide ? sectionFromHash(hash) : null

  const shell = useMemo<Shell>(() => ({ query, setQuery, setToc }), [query])

  /**
   * `/` focuses the filter, from anywhere, the same key the ledgers use.
   * Ignored while an input has focus.
   *
   * It is taken in the capture phase, because a documentation page can
   * contain a live `Ledger` whose own `/` would otherwise swallow it. The
   * routes below are the exception: they are a whole product surface, and
   * their keyboard model — `/`, `j`/`k`, digits — is the thing on display,
   * so on those the shell keeps its hands off the key.
   */
  useEffect(() => {
    if (PAGE_OWNS_SLASH.has(location.pathname)) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return
      const t = e.target as HTMLElement | null
      if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return
      e.preventDefault()
      e.stopPropagation()
      filterRef.current?.focus()
      filterRef.current?.select()
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [location.pathname])

  /**
   * Land on the deep link. The browser scrolls to a fragment once, at load,
   * and the reference pages are full of charts and ledgers that finish
   * measuring a frame or two later — so `#chart` would otherwise leave the
   * reader a screen and a half past the block they asked for. Re-land only
   * while the target is out of place and the reader has not scrolled away.
   */
  const landedAt = useRef<number | null>(null)
  useEffect(() => {
    const id = decodeURIComponent(hash.replace(/^#/, ''))
    landedAt.current = null
    if (!id) return
    const land = () => {
      const el = document.getElementById(id)
      if (!el) return
      const margin = Number.parseFloat(getComputedStyle(el).scrollMarginTop) || 0
      if (Math.abs(el.getBoundingClientRect().top - margin) < 4) return
      if (landedAt.current !== null && Math.abs(window.scrollY - landedAt.current) > 4) return
      el.scrollIntoView({ block: 'start' })
      landedAt.current = window.scrollY
    }
    const raf = requestAnimationFrame(land)
    const soon = window.setTimeout(land, 250)
    const later = window.setTimeout(land, 900)
    return () => {
      cancelAnimationFrame(raf)
      window.clearTimeout(soon)
      window.clearTimeout(later)
    }
  }, [hash, location.pathname])

  const needle = query.trim().toLowerCase()
  const match = (label: string) => !needle || label.toLowerCase().includes(needle)
  const guideEntries = useMemo(
    () => GUIDE_NAV.map((s, i) => ({ ...s, n: i + 1 })).filter((s) => match(s.label)),
    [needle],
  )
  const refEntries = useMemo(() => REFERENCE.filter((r) => match(r.label)), [needle])

  const guideHref = (id: string) => (onGuide ? `#${id}` : `/guide#${id}`)

  const railLink = (isCurrent: boolean) =>
    cn(
      'flex items-baseline gap-2 px-2 py-1',
      isCurrent ? 'bg-muted font-medium' : 'text-muted-foreground hover:text-foreground',
    )

  return (
    <ShellContext.Provider value={shell}>
      <div className="operator ink v1 min-h-screen bg-background text-foreground">
        <header className="op-sticky sticky top-0 z-30 flex h-12 items-center gap-3 border-b bg-background px-3 sm:px-4">
          <LogoMark size={18} />
          <h1 className="shrink-0 text-sm font-semibold tracking-tight">Temps design system</h1>
          <span className="hidden text-xs text-muted-foreground lg:block">
            guide, components and console mockups on @temps-sdk/op
          </span>
          <div className="ml-auto flex items-center gap-2">
            <label className="flex h-7 items-center gap-2 border px-2 focus-within:outline-2 focus-within:-outline-offset-2 focus-within:outline-ring">
              <span className="sr-only">Filter the design system</span>
              <input
                ref={filterRef}
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Escape') {
                    setQuery('')
                    e.currentTarget.blur()
                  }
                }}
                placeholder="filter"
                className="w-24 bg-transparent font-mono text-xs outline-none placeholder:text-muted-foreground sm:w-56"
              />
              <Kbd keys="/" className="hidden sm:flex" />
            </label>
            <ThemeToggle />
          </div>
        </header>

        {/* Below lg the rail becomes one scrolling row, so a phone loses width, never function. */}
        <nav
          aria-label="Sections, compact"
          className="op-scroll-x flex items-baseline gap-4 border-b px-3 py-2 text-xs lg:hidden"
        >
          {guideEntries.length > 0 && <span className="op-label shrink-0 text-muted-foreground">guide</span>}
          {guideEntries.map((s) => (
            <a
              key={s.id}
              href={guideHref(s.id)}
              aria-current={s.id === currentGuide ? 'true' : undefined}
              className={cn(
                'shrink-0 whitespace-nowrap',
                s.id === currentGuide ? 'font-medium underline underline-offset-4' : 'text-muted-foreground',
              )}
            >
              {s.label}
            </a>
          ))}
          {refEntries.length > 0 && <span className="op-label shrink-0 text-muted-foreground">reference</span>}
          {refEntries.map((r) => (
            <Link
              key={r.to}
              to={r.to}
              aria-current={r.to === location.pathname ? 'true' : undefined}
              className={cn(
                'shrink-0 whitespace-nowrap',
                r.to === location.pathname
                  ? 'font-medium underline underline-offset-4'
                  : 'text-muted-foreground',
              )}
            >
              {r.label}
            </Link>
          ))}
        </nav>

        <div className="flex items-start">
          <nav
            aria-label="Design system sections"
            className="sticky top-12 hidden max-h-[calc(100vh-3rem)] w-56 shrink-0 self-start overflow-y-auto border-r p-3 text-xs lg:block"
          >
            <p className="op-label mb-2 text-muted-foreground">guide</p>
            <ol className="space-y-0.5">
              {guideEntries.map((s) => (
                <li key={s.id}>
                  <a
                    href={guideHref(s.id)}
                    aria-current={s.id === currentGuide ? 'true' : undefined}
                    className={railLink(s.id === currentGuide)}
                  >
                    <span className="w-4 shrink-0 font-mono text-[11px] tabular-nums">{s.n}</span>
                    <span className="min-w-0">{s.label}</span>
                  </a>
                </li>
              ))}
            </ol>

            <p className="op-label mb-2 mt-5 text-muted-foreground">reference</p>
            <ul className="space-y-0.5">
              {refEntries.map((r) => (
                <li key={r.to}>
                  <Link
                    to={r.to}
                    aria-current={r.to === location.pathname ? 'true' : undefined}
                    className={railLink(r.to === location.pathname)}
                  >
                    <span className="min-w-0">{r.label}</span>
                  </Link>
                </li>
              ))}
            </ul>

            {guideEntries.length === 0 && refEntries.length === 0 && (
              <p className="mt-4 text-muted-foreground">Nothing in the rail matches “{query.trim()}”.</p>
            )}
          </nav>

          <main className="min-w-0 flex-1 px-4 py-6 sm:px-6">{children}</main>

          {/* Always drawn, always the same width, so the column never shifts
              between a page that has an "on this page" list and one that does
              not. Its parent is the row, not a wrapper, or the sticky would
              have nothing to stick inside. */}
          <nav
            aria-label="On this page"
            className="sticky top-12 hidden max-h-[calc(100vh-3rem)] w-52 shrink-0 self-start overflow-y-auto p-4 text-xs xl:block"
          >
            {toc.length > 0 && (
              <>
                <p className="op-label mb-2 text-muted-foreground">on this page</p>
                <ul className="space-y-1">
                  {toc.map((e) => (
                    <li key={e.id}>
                      <a href={`#${e.id}`} className="block truncate text-muted-foreground hover:text-foreground">
                        {e.text}
                      </a>
                    </li>
                  ))}
                </ul>
              </>
            )}
          </nav>
        </div>
      </div>
    </ShellContext.Provider>
  )
}
