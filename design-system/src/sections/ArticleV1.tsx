// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, type ReactNode } from 'react'
import { Link } from 'react-router'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import {
  Article, Callout, CodeBlock, ImageFigure, Kbd, ProjectMark, TimeChart,
  type Marker, type Series, type State, type TimePoint,
} from '@/components/op'
import { PAGE_BLEED } from '@/components/shell-context'
import { LandingHeader } from '@/sections/InkLandingV1'

import postMd from '../../content/sample-post.md?raw'

/* ────────────────────────────────────────────────────────────────────────
   /article — the worked example of the fourth page template.

   The marketing site's blog posts are about 1,500 words with a dozen
   headings, a table of contents, a screenshot and no figures. This is one
   of them, written as the real thing (`design-system/content/sample-post.md`)
   and rendered through `Article` + `.op-prose`, so the template is shown
   carrying a document rather than lorem.

   `/article` is the page as it would ship — the landing's own header above
   it and nothing else. `/v1-article` is the same page inside the sandbox
   chrome. The landing's `Blog` nav item points at `/v1-article`, so the
   example is reachable from where a reader would look for it.

   MDX is what the real site authors in. React-markdown has no components in
   its syntax, so the two things a post needs that markdown has no word for —
   a chart and a callout — arrive as fenced blocks with a language and a JSON
   body. That is the smallest vocabulary that works in both: MDX writes
   `<TimeChart …/>`, this writes ```chart, and both land on the same
   primitive. Nothing here invents a look; every block is a component from
   `@temps-sdk/op`.
   ──────────────────────────────────────────────────────────────────────── */

/** The `key: value` block at the top of the file, and the body after it. */
function frontmatter(src: string): { meta: Record<string, string>; body: string } {
  const m = /^---\n([\s\S]*?)\n---\n?/.exec(src)
  if (!m) return { meta: {}, body: src }
  const meta: Record<string, string> = {}
  for (const line of m[1].split('\n')) {
    const at = line.indexOf(':')
    if (at < 0) continue
    meta[line.slice(0, at).trim()] = line.slice(at + 1).trim()
  }
  return { meta, body: src.slice(m[0].length) }
}

/** A fence's JSON body, or null when it does not parse. A bad fence says so. */
function parse<T>(text: string): T | null {
  try {
    return JSON.parse(text) as T
  } catch {
    return null
  }
}

type ChartFence = {
  title: string
  range: string
  verdict?: string
  unit?: string
  series: Series[]
  markers?: Marker[]
  data: TimePoint[]
}
type FigureFence = { src: string; dark?: string; alt: string; caption: string; width: number; height: number }
type CalloutFence = { state: State; title: string; body?: string; quote?: string }

/** The text inside a `<code>` node, however react-markdown nested it. */
function textOf(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === 'boolean') return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  const el = node as { props?: { children?: ReactNode } }
  return el.props ? textOf(el.props.children) : ''
}

/**
 * A fence whose language the post has a component for. Everything else falls
 * through to `CodeBlock`, which is the right answer for code: a document's
 * code exists to be run, so it is copyable and it says what it is.
 */
function Fence({ lang, meta, code }: { lang: string; meta: string; code: string }) {
  if (lang === 'chart') {
    const c = parse<ChartFence>(code)
    if (!c) return <CodeBlock lang="chart" code={code} />
    return (
      <figure className="mt-6 min-w-0">
        <TimeChart
          data={c.data}
          series={c.series}
          markers={c.markers}
          unit={c.unit}
          title={c.title}
          range={c.range}
          verdict={c.verdict}
        />
        <figcaption>{c.title} · {c.range}</figcaption>
      </figure>
    )
  }
  if (lang === 'figure') {
    const f = parse<FigureFence>(code)
    if (!f) return <CodeBlock lang="figure" code={code} />
    return <ImageFigure src={f.src} dark={f.dark} alt={f.alt} caption={f.caption} width={f.width} height={f.height} />
  }
  if (lang === 'callout') {
    const c = parse<CalloutFence>(code)
    if (!c) return <CodeBlock lang="callout" code={code} />
    return (
      <div className="mt-6 max-w-[var(--op-measure)]">
        <Callout state={c.state} title={c.title} quote={c.quote}>{c.body}</Callout>
      </div>
    )
  }
  // `bash filename=…` — the filename rides on the fence's info string, the
  // way every MDX pipeline spells it.
  const filename = /(?:filename|title)=["']?([^"'\s]+)/.exec(meta)?.[1]
  return <CodeBlock lang={lang || 'text'} code={code} filename={filename} />
}

/**
 * The component map. It is deliberately short: `.op-prose` is the whole look
 * of the body, so there is nothing here that sets a margin, a size or a
 * colour. What is left is the three things a class cannot do — route a link,
 * turn a fence into a component, and turn `kbd:…` into a real key badge.
 */
/**
 * A GFM `---:` column is a right-aligned column, which in the ledger idiom
 * means a number: `data-align="end"` sets it in mono and tabular. The
 * attribute is the documented way to say so; markdown's alignment is just
 * how a post spells it.
 */
function align(style?: { textAlign?: string }): 'end' | undefined {
  return style?.textAlign === 'right' ? 'end' : undefined
}

const COMPONENTS: Components = {
  th: ({ children, style }) => <th scope="col" data-align={align(style)}>{children}</th>,
  td: ({ children, style }) => <td data-align={align(style)}>{children}</td>,
  // GFM renders a task list as a disabled checkbox with nothing to name it.
  // The box is the state, so it says the state: `done` / `not done`.
  input: ({ checked, type }) =>
    type === 'checkbox'
      ? <input type="checkbox" checked={!!checked} disabled readOnly aria-label={checked ? 'done' : 'not done'} />
      : null,
  a: ({ href, children }) => {
    const h = href ?? ''
    if (h.startsWith('/')) return <Link to={h}>{children}</Link>
    if (/^https?:/.test(h)) return <a href={h} target="_blank" rel="noreferrer">{children}</a>
    return <a href={h}>{children}</a>
  },
  // A key is a badge, not a word in a sentence. Markdown has no syntax for
  // one, so inline code that starts with `kbd:` becomes the badge `Kbd`
  // draws; `.op-prose kbd` styles it the same either way.
  code: ({ children, className, ...rest }) => {
    const text = textOf(children)
    if (!className && text.startsWith('kbd:')) return <Kbd keys={text.slice(4).split('+')} />
    return <code className={className} {...rest}>{children}</code>
  },
  // The fence arrives as <pre><code class="language-x">; react-markdown puts
  // the info string on the `pre` node, so the swap happens here.
  pre: ({ children, node }) => {
    const child = Array.isArray(children) ? children[0] : children
    const el = child as { props?: { className?: string; children?: ReactNode } } | undefined
    const lang = /language-([\w-]+)/.exec(el?.props?.className ?? '')?.[1] ?? ''
    const meta = String((node?.children?.[0] as { data?: { meta?: string } } | undefined)?.data?.meta ?? '')
    return <Fence lang={lang} meta={meta} code={textOf(el?.props?.children).replace(/\n$/, '')} />
  },
}

/**
 * The post, rendered. Exported so the guide can show it in a live frame.
 *
 * `backHref` is where "all posts" goes. The sandbox has no posts index, so it
 * goes to the landing the post belongs to — a typed destination, never `#`.
 * A real index page is the follow-up.
 */
export function SamplePost({ backHref = '/v1-landing' }: { backHref?: string }) {
  const { meta, body } = useMemo(() => frontmatter(postMd), [])
  return (
    <Article
      title={meta.title ?? 'Untitled'}
      lede={meta.lede}
      author={{ name: meta.author ?? 'maya', mark: <ProjectMark name={meta.author ?? 'maya'} size={16} /> }}
      date={meta.date ?? '2026-09-01'}
      readingMinutes={Number(meta.readingMinutes) || undefined}
      backHref={backHref}
      backLabel="all posts"
      footer={<>Written against the Temps proxy log. The queries in it run unchanged on a self-hosted instance.</>}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={COMPONENTS}>{body}</ReactMarkdown>
    </Article>
  )
}

export function ArticleV1Page({ full = false }: { /** Render without the sandbox layout: the post as it would ship. Route `/article`. */ full?: boolean }) {
  return (
    <div className={full ? 'operator ink v1 min-h-screen' : `operator ink v1 ${PAGE_BLEED}`}>
      <LandingHeader />
      <SamplePost backHref={full ? '/landing' : '/v1-landing'} />
    </div>
  )
}
