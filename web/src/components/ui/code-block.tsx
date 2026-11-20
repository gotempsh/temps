// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Hash, WrapText } from 'lucide-react'
import { cn } from '@/lib/utils'
import { scrollableRegionProps, useScrollableRegion } from './scrollable-region'
import type { HighlighterCore } from 'shiki/core'
import { withReadableDarkComments } from './code-block-theme'

/**
 * Languages this block can highlight.
 *
 * Adding one means adding its grammar to `LANG_LOADERS` below — the list is
 * explicit rather than shiki's full bundle because that bundle is several
 * megabytes of grammars for languages the console will never render.
 */
export type CodeLanguage =
  | 'bash'
  | 'shell'
  | 'yaml'
  | 'json'
  | 'javascript'
  | 'typescript'
  | 'tsx'
  | 'text'
  | 'python'
  | 'go'
  | 'rust'
  | 'ruby'
  | 'php'
  | 'java'
  | 'sql'
  | 'dockerfile'
  | 'css'
  | 'html'
  | 'markdown'
  | 'toml'
  | 'xml'

interface CodeBlockProps {
  code: string
  language: CodeLanguage
  className?: string
  showCopy?: boolean
  title?: string
  defaultWrap?: boolean
  defaultShowLineNumbers?: boolean
  disableWrapToggle?: boolean
}

// Same themes the docs site compiles its MDX with, so a snippet reads
// identically whether it's on temps.sh/docs or inside the console.
const THEMES = { light: 'github-light', dark: 'github-dark' } as const

const LANG_LOADERS: Record<
  Exclude<CodeLanguage, 'text'>,
  () => Promise<unknown>
> = {
  bash: () => import('shiki/langs/bash.mjs'),
  shell: () => import('shiki/langs/shellscript.mjs'),
  yaml: () => import('shiki/langs/yaml.mjs'),
  json: () => import('shiki/langs/json.mjs'),
  javascript: () => import('shiki/langs/javascript.mjs'),
  typescript: () => import('shiki/langs/typescript.mjs'),
  tsx: () => import('shiki/langs/tsx.mjs'),
  python: () => import('shiki/langs/python.mjs'),
  go: () => import('shiki/langs/go.mjs'),
  rust: () => import('shiki/langs/rust.mjs'),
  ruby: () => import('shiki/langs/ruby.mjs'),
  php: () => import('shiki/langs/php.mjs'),
  java: () => import('shiki/langs/java.mjs'),
  sql: () => import('shiki/langs/sql.mjs'),
  dockerfile: () => import('shiki/langs/docker.mjs'),
  css: () => import('shiki/langs/css.mjs'),
  html: () => import('shiki/langs/html.mjs'),
  markdown: () => import('shiki/langs/markdown.mjs'),
  toml: () => import('shiki/langs/toml.mjs'),
  xml: () => import('shiki/langs/xml.mjs'),
}

// One highlighter for the whole app, built lazily on the first code block and
// grown one grammar at a time. Loading every grammar up front would pull
// megabytes into the initial bundle for pages that show no code at all.
let highlighterPromise: Promise<HighlighterCore> | null = null
const loadedLangs = new Set<string>()

async function getHighlighter(): Promise<HighlighterCore> {
  if (!highlighterPromise) {
    highlighterPromise = (async () => {
      const [{ createHighlighterCore }, { createJavaScriptRegexEngine }] =
        await Promise.all([
          import('shiki/core'),
          import('shiki/engine/javascript'),
        ])
      // The JS engine keeps us off the 450 KB oniguruma wasm blob, which
      // rsbuild would have to serve as a separate asset.
      return createHighlighterCore({
        themes: [
          import('shiki/themes/github-light.mjs'),
          import('shiki/themes/github-dark.mjs').then((module) =>
            withReadableDarkComments(module.default)
          ),
        ],
        langs: [],
        engine: createJavaScriptRegexEngine({ forgiving: true }),
      })
    })()
  }
  return highlighterPromise
}

async function highlight(
  code: string,
  language: CodeLanguage
): Promise<string | null> {
  if (language === 'text') return null

  const highlighter = await getHighlighter()
  if (!loadedLangs.has(language)) {
    await highlighter.loadLanguage(
      (await LANG_LOADERS[language]()) as Parameters<
        typeof highlighter.loadLanguage
      >[0]
    )
    loadedLangs.add(language)
  }

  const html = highlighter.codeToHtml(code, {
    lang: language,
    themes: THEMES,
    // Emits --shiki-light / --shiki-dark custom properties instead of baking
    // one theme's colours in, so the block follows the console's theme
    // toggle without re-highlighting. globals.css maps the vars to `color`.
    defaultColor: false,
  })

  // Shiki hands back a full <pre><code>…</code></pre>; we only want the token
  // markup, since the surrounding chrome is ours.
  return (
    html.match(/<pre[^>]*><code[^>]*>([\s\S]*?)<\/code><\/pre>/)?.[1] ?? null
  )
}

export function CodeBlock({
  code,
  language = 'text',
  className,
  showCopy = true,
  title,
  defaultWrap = false,
  defaultShowLineNumbers = false,
  disableWrapToggle = false,
}: CodeBlockProps) {
  const [wrapLines, setWrapLines] = useState(defaultWrap)
  const [showLineNumbers, setShowLineNumbers] = useState(defaultShowLineNumbers)

  // Long unwrapped lines scroll horizontally; make that scroller a tab stop
  // (only while it overflows) so keyboard users can reach the rest of the line.
  const { ref: scrollerRef, scrollable } = useScrollableRegion<HTMLDivElement>()
  const scrollerProps = scrollableRegionProps(scrollable)

  const controls = (className: string) => (
    <div className={cn('flex items-center gap-0.5', className)}>
      <Button
        size="icon"
        variant="ghost"
        className={cn(
          'size-7 rounded-md',
          'text-muted-foreground hover:text-foreground',
          'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
        )}
        onClick={() => setShowLineNumbers(!showLineNumbers)}
        title={showLineNumbers ? 'Hide line numbers' : 'Show line numbers'}
        aria-label={showLineNumbers ? 'Hide line numbers' : 'Show line numbers'}
      >
        <Hash
          className={cn(
            'size-3.5',
            showLineNumbers && 'text-blue-500 dark:text-blue-400'
          )}
        />
      </Button>
      {!disableWrapToggle && (
        <Button
          size="icon"
          variant="ghost"
          className={cn(
            'size-7 rounded-md',
            'text-muted-foreground hover:text-foreground',
            'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
          )}
          onClick={() => setWrapLines(!wrapLines)}
          title={wrapLines ? 'Disable line wrap' : 'Enable line wrap'}
          aria-label={wrapLines ? 'Disable line wrap' : 'Enable line wrap'}
        >
          <WrapText
            className={cn(
              'size-3.5',
              wrapLines && 'text-blue-500 dark:text-blue-400'
            )}
          />
        </Button>
      )}
      {showCopy && (
        // CopyButton falls back to a selection copy where the Clipboard API
        // is missing (plain-http instances) and reports a failed copy.
        <CopyButton
          value={code}
          label="Copy"
          className={cn(
            'size-7 rounded-md [&>svg]:size-3.5',
            'text-muted-foreground hover:text-foreground',
            'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
          )}
        />
      )}
    </div>
  )

  return (
    // Container, header and type scale are lifted verbatim from the docs site's
    // `CodeGroup` (temps-landing `src/components/Code.tsx`): a rounded-2xl
    // panel on zinc-900 in dark with a hairline ring, rather than a bordered
    // zinc-950 box. Same snippet, same surface, whether you're reading
    // temps.sh/docs or the console.
    <div
      className={cn(
        'group group/code relative overflow-hidden rounded-2xl bg-white shadow-md ring-1 ring-zinc-900/[0.075]',
        'dark:bg-zinc-900 dark:ring-white/10',
        className
      )}
    >
      {title && (
        // A titled block carries its controls in the title bar, so the code
        // starts right under it instead of below an empty toolbar row.
        <div className="flex h-9 items-center justify-between gap-2 border-b border-zinc-900/[0.075] bg-zinc-900/[0.025] pr-1 pl-4 font-mono text-xs text-muted-foreground dark:border-b-white/5 dark:bg-white/[0.01]">
          <span className="min-w-0 truncate">{title}</span>
          {controls('shrink-0')}
        </div>
      )}
      {/* The white/2.5 wash over zinc-900 is what gives the dark panel its
          slightly-lifted tone in the docs; without it the block reads as a
          flat hole in the page. */}
      <div className="flex min-w-0 flex-col dark:bg-white/[0.025]">
        {/* Untitled blocks look like the docs: the code starts at the top and
            the controls float in the corner, shown on hover or keyboard
            focus, and always on touch screens, which have no hover. They
            sit on the panel colour so a long line never shows through. */}
        {!title &&
          controls(
            cn(
              'absolute top-2 right-2 z-10 rounded-lg bg-white/90 p-0.5 backdrop-blur-sm dark:bg-zinc-900/90',
              'opacity-0 transition-opacity group-hover/code:opacity-100 group-focus-within/code:opacity-100 [@media(hover:none)]:opacity-100'
            )
          )}
        <div
          ref={scrollerRef}
          {...scrollerProps}
          className={cn(
            'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
            'min-w-0 w-full px-4 py-4',
            wrapLines ? 'overflow-x-hidden' : 'overflow-x-auto'
          )}
        >
          <pre
            className={cn(
              'm-0 font-mono text-[13px] leading-6',
              // Wrap at spaces first; split a token only when it alone is too long.
              wrapLines
                ? 'whitespace-pre-wrap [overflow-wrap:anywhere]'
                : 'whitespace-pre'
            )}
          >
            <HighlightedCode
              code={code}
              language={language}
              showLineNumbers={showLineNumbers}
            />
          </pre>
        </div>
      </div>
    </div>
  )
}

/**
 * Syntax-coloured code without panel chrome or controls. This lets compact
 * command surfaces share the same lazy Shiki instance and theme as CodeBlock.
 */
export function HighlightedCode({
  code,
  language = 'text',
  className,
  showLineNumbers = false,
}: {
  code: string
  language: CodeLanguage
  className?: string
  showLineNumbers?: boolean
}) {
  const highlightKey = `${language}\u0000${code}`
  const [highlighted, setHighlighted] = useState<{
    key: string
    html: string | null
  } | null>(null)
  const html = highlighted?.key === highlightKey ? highlighted.html : null

  useEffect(() => {
    let cancelled = false
    highlight(code, language)
      .then((result) => {
        if (!cancelled) setHighlighted({ key: highlightKey, html: result })
      })
      // A grammar that fails to load must not blank the snippet — the plain
      // fallback below is already rendered and stays.
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [code, highlightKey, language])

  const codeClassName = cn(
    'shiki-code',
    !html && 'text-foreground dark:text-zinc-100',
    showLineNumbers && 'shiki-line-numbers',
    className
  )

  if (html) {
    // Shiki's output is HTML-escaped token markup — the only markup it emits
    // around source text is a span carrying theme colour variables.
    return (
      <code
        className={codeClassName}
        dangerouslySetInnerHTML={{ __html: html }}
      />
    )
  }

  return (
    <code className={codeClassName}>
      {code.split('\n').map((line, index, lines) => (
        <span key={index} className="line">
          {line}
          {index < lines.length - 1 ? '\n' : ''}
        </span>
      ))}
    </code>
  )
}

// Export a variant for inline code
export function InlineCode({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return (
    <code
      className={cn(
        'px-1.5 py-0.5 rounded bg-muted font-mono text-sm',
        className
      )}
    >
      {children}
    </code>
  )
}
