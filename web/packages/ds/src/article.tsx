// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { ArrowLeft } from 'lucide-react'
import { cn } from './lib/cn'
import { CopyAction } from './copy'
import { fmtAbsolute } from './fmt'

/* ────────────────────────────────────────────────────────────────────────
   Article — the fourth page template, for a page that is read top to
   bottom rather than operated: a blog post, a docs page, a changelog
   entry, a compare page.

   The console templates (Ledger, Detail, Settings) are about finding one
   record among many and doing something to it. A read is the opposite
   shape: one column at a fixed measure, headings as the joints, and a
   rail that says where in it you are. Everything else is the same
   system — `.op-prose` is the whole look of the body, so an MDX page gets
   it from one class and there is one copy of the rules.

   See design-system/docs/design-system-handoff.md §7 (template 4) and
   design-system/docs/content-pages.md.
   ──────────────────────────────────────────────────────────────────────── */

export type ArticleAuthor = {
  /** The name as the person writes it. Never re-cased. */
  name: string
  /** An identity mark beside the name — 16px, the way every other mark sits. */
  mark?: ReactNode
}

/** One entry in the right rail: a heading in the body, and how deep it is. */
export type ArticleHeading = { id: string; text: string; level: 2 | 3 }

/** `A title, like this` → `a-title-like-this`. Stable enough to link to. */
function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\p{Letter}\p{Number}]+/gu, '-')
    .replace(/^-+|-+$/g, '')
}

/**
 * The table of contents is read out of the *rendered* body, not passed in.
 *
 * A markdown or MDX page is authored as prose; asking the author to keep a
 * second list of its own headings in sync is a list that goes stale on the
 * first edit and takes the reader to the wrong place. So the headings are
 * whatever the body actually rendered, and a heading with no id gets one.
 */
function useHeadings(root: React.RefObject<HTMLElement | null>, enabled: boolean): ArticleHeading[] {
  const [items, setItems] = useState<ArticleHeading[]>([])
  useEffect(() => {
    const el = root.current
    if (!enabled || !el) return
    const read = () => {
      const found: ArticleHeading[] = []
      const seen = new Set<string>()
      for (const h of Array.from(el.querySelectorAll<HTMLElement>('h2, h3'))) {
        const text = (h.textContent ?? '').trim()
        if (!text) continue
        let id = h.id
        if (!id) {
          id = slug(text) || `section-${found.length + 1}`
          let n = 2
          while (seen.has(id)) id = `${slug(text)}-${n++}`
          h.id = id
        }
        seen.add(id)
        h.classList.add('scroll-mt-16')
        found.push({ id, text, level: h.tagName === 'H2' ? 2 : 3 })
      }
      setItems((prev) =>
        prev.length === found.length && prev.every((p, i) => p.id === found[i].id) ? prev : found,
      )
    }
    read()
    const mo = new MutationObserver(read)
    mo.observe(el, { childList: true, subtree: true })
    return () => mo.disconnect()
  }, [root, enabled])
  return items
}

/**
 * Which heading the reader is under. The last heading whose top has passed
 * the top quarter of the viewport, which is what "where am I" means while
 * scrolling — an IntersectionObserver alone marks nothing when a long
 * section fills the screen.
 */
function useCurrent(items: ArticleHeading[]): string | undefined {
  const [current, setCurrent] = useState<string | undefined>(undefined)
  useEffect(() => {
    if (items.length === 0) return
    let frame = 0
    const measure = () => {
      frame = 0
      const line = window.innerHeight * 0.25
      let active = items[0]?.id
      for (const it of items) {
        const el = document.getElementById(it.id)
        if (!el) continue
        if (el.getBoundingClientRect().top <= line) active = it.id
      }
      setCurrent(active)
    }
    const onScroll = () => { if (!frame) frame = window.requestAnimationFrame(measure) }
    measure()
    window.addEventListener('scroll', onScroll, { passive: true })
    window.addEventListener('resize', onScroll)
    return () => {
      window.cancelAnimationFrame(frame)
      window.removeEventListener('scroll', onScroll)
      window.removeEventListener('resize', onScroll)
    }
  }, [items])
  return current
}

/**
 * A page that is read top to bottom.
 *
 * `title` is the one display line on the page. `lede` is the sentence under
 * it, at the lead size. The byline is mark · name · the absolute date · the
 * reading time, in that order and as one mono line: a reader deciding
 * whether to start wants who, when and how long, and nothing else.
 *
 * `toc` builds the right rail from the body's own h2/h3. It is sticky, the
 * current section is ink and every entry is a real link, so the rail works
 * from the keyboard and every heading is addressable.
 */
export function Article({
  title, lede, author, date, readingMinutes, toc = true, children, aside, footer,
  backHref, backLabel = 'all posts', className,
}: {
  title: string
  lede?: ReactNode
  author: ArticleAuthor
  /** The publication date. Rendered absolute — a post is not "3h ago". */
  date: Date | string
  /** Whole minutes. Say it once, in the byline. */
  readingMinutes?: number
  toc?: boolean
  children: ReactNode
  /** Extra rail content under the contents (related reading, a note). */
  aside?: ReactNode
  /** What goes under the rule at the end: the next step, a link, a note. */
  footer?: ReactNode
  /** Where "back to all posts" goes. */
  backHref?: string
  backLabel?: string
  className?: string
}) {
  const body = useRef<HTMLDivElement>(null)
  const items = useHeadings(body, toc)
  const current = useCurrent(items)
  const when = useMemo(() => (date instanceof Date ? date : new Date(date)), [date])
  const showRail = toc && items.length > 1

  return (
    <article className={cn('mx-auto w-full max-w-6xl px-4 py-10 sm:px-8', className)}>
      {backHref ? (
        <p className="mb-8">
          <a href={backHref} className="op-label inline-flex items-center gap-1.5 text-muted-foreground hover:text-foreground">
            <ArrowLeft aria-hidden className="h-3.5 w-3.5" /> {backLabel}
          </a>
        </p>
      ) : null}

      <header className="max-w-[var(--op-measure)]">
        <h1 className="op-h1">{title}</h1>
        {lede ? <p className="op-lede">{lede}</p> : null}
        <p className="mt-5 flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs text-muted-foreground">
          {author.mark ? <span className="inline-flex size-4 shrink-0 items-center">{author.mark}</span> : null}
          <span className="text-foreground">{author.name}</span>
          <span aria-hidden>·</span>
          <time dateTime={when.toISOString()}>{fmtAbsolute(when, { time: false })}</time>
          {readingMinutes ? (
            <>
              <span aria-hidden>·</span>
              <span>{readingMinutes} min read</span>
            </>
          ) : null}
        </p>
      </header>

      <div className={cn('mt-8 grid gap-x-10 gap-y-8', showRail && 'lg:grid-cols-[minmax(0,1fr)_15rem]')}>
        <div ref={body} className="op-prose min-w-0">{children}</div>
        {showRail ? (
          <nav aria-label="On this page" className="min-w-0 lg:order-last">
            <div className="sticky top-6 border-t pt-3 lg:border-t-0 lg:border-l lg:pl-4 lg:pt-0">
              <p className="op-label text-muted-foreground">on this page</p>
              <ul className="mt-2 space-y-1.5 text-xs">
                {items.map((it) => (
                  <li key={it.id} className={cn(it.level === 3 && 'ps-3')}>
                    <a
                      href={`#${it.id}`}
                      aria-current={current === it.id ? 'true' : undefined}
                      className={cn(
                        'block hover:text-foreground',
                        current === it.id ? 'font-medium text-foreground' : 'text-muted-foreground',
                      )}
                    >
                      {it.text}
                    </a>
                  </li>
                ))}
              </ul>
              {aside ? <div className="mt-6 border-t pt-3">{aside}</div> : null}
            </div>
          </nav>
        ) : aside ? (
          <aside className="min-w-0">{aside}</aside>
        ) : null}
      </div>

      {(footer || backHref) && (
        <footer className="mt-12 border-t pt-5">
          {footer ? <div className="max-w-[var(--op-measure)] text-sm text-muted-foreground">{footer}</div> : null}
          {backHref ? (
            <p className={cn(footer && 'mt-4')}>
              <a href={backHref} className="op-label inline-flex items-center gap-1.5 text-muted-foreground hover:text-foreground">
                <ArrowLeft aria-hidden className="h-3.5 w-3.5" /> back to {backLabel}
              </a>
            </p>
          ) : null}
        </footer>
      )}
    </article>
  )
}

/**
 * A code block with a label row: the language, the filename when the code
 * belongs to one, and a `copy` on the right.
 *
 * Code in a document exists to be run, so it is copyable by default and the
 * copy answers on the button — never in a toast that says "copied" whether
 * or not anything reached the clipboard. The pane is the inset tone, the
 * same one the console's log and code panes use, and it scrolls sideways
 * rather than wrapping a command across two lines.
 */
export function CodeBlock({
  code, lang, filename, copy = true, className,
}: {
  code: string
  /** The language word, shown in the label row: `bash`, `ts`, `sql`. */
  lang: string
  filename?: string
  copy?: boolean
  className?: string
}) {
  const label = filename ?? lang
  return (
    <div className={cn('mt-4 border', className)}>
      <div className="flex items-center gap-2 border-b px-3 py-1.5">
        <span className="op-label text-muted-foreground">{lang}</span>
        {filename ? <span className="truncate font-mono text-[11px]">{filename}</span> : null}
        {copy ? (
          <CopyAction value={code} className="ms-auto h-6" aria-label={`Copy ${label}`}>
            copy
          </CopyAction>
        ) : null}
      </div>
      <pre tabIndex={0} data-allow-overflow className="op-inset m-0 overflow-x-auto border-0 p-3 font-mono text-[12px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
        <code>{code}</code>
      </pre>
    </div>
  )
}

/**
 * A picture in a document: one 1px ink frame, no rounding, no shadow, and
 * always an alt and a caption.
 *
 * A screenshot taken on paper is unreadable on night and the other way
 * round, so a figure takes both and swaps them with the theme; a page that
 * ships one is a page that is broken in half its states. The caption is
 * numbered by `.op-prose`'s figure counter, so the prose can say "fig. 3"
 * and mean it. `width` and `height` are the intrinsic pixels: they reserve
 * the box so the text under the figure does not jump when it loads.
 */
export function ImageFigure({
  src, dark, alt, caption, width, height, className,
}: {
  src: string
  /** The night-mode file. Omit only when the picture has no theme. */
  dark?: string
  alt: string
  caption: ReactNode
  width: number
  height: number
  className?: string
}) {
  return (
    <figure className={cn('min-w-0', className)}>
      <img src={src} alt={alt} width={width} height={height} data-theme={dark ? 'light' : undefined} />
      {dark ? <img src={dark} alt={alt} width={width} height={height} data-theme="dark" /> : null}
      <figcaption>{caption}</figcaption>
    </figure>
  )
}
