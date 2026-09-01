// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Check, Copy, Hash, WrapText } from 'lucide-react'
import { cn } from '@/lib/utils'
import type { HighlighterCore } from 'shiki/core'

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

interface CodeBlockProps {
  code: string
  language?: CodeLanguage
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
          import('shiki/themes/github-dark.mjs'),
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
  const [copied, setCopied] = useState(false)
  const [wrapLines, setWrapLines] = useState(defaultWrap)
  const [showLineNumbers, setShowLineNumbers] = useState(defaultShowLineNumbers)

  const visibleButtonCount =
    1 + (disableWrapToggle ? 0 : 1) + (showCopy ? 1 : 0)

  const handleCopy = async () => {
    await navigator.clipboard.writeText(code)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  return (
    // Container, header and type scale are lifted verbatim from the docs site's
    // `CodeGroup` (temps-landing `src/components/Code.tsx`): a rounded-2xl
    // panel on zinc-900 in dark with a hairline ring, rather than a bordered
    // zinc-950 box. Same snippet, same surface, whether you're reading
    // temps.sh/docs or the console.
    <div
      className={cn(
        'group overflow-hidden rounded-2xl bg-white shadow-md ring-1 ring-zinc-900/[0.075]',
        'dark:bg-zinc-900 dark:ring-white/10',
        className
      )}
    >
      {title && (
        <div className="flex h-9 items-center gap-2 border-b border-zinc-900/[0.075] bg-zinc-900/[0.025] px-4 font-mono text-xs text-muted-foreground dark:border-b-white/5 dark:bg-white/[0.01]">
          {title}
        </div>
      )}
      {/* The white/2.5 wash over zinc-900 is what gives the dark panel its
          slightly-lifted tone in the docs; without it the block reads as a
          flat hole in the page. */}
      <div className="relative min-w-0 dark:bg-white/[0.025]">
        <div
          className={cn(
            // w-0 min-w-full max-w-full is the trick: forces this element to
            // parent width while letting its children overflow horizontally.
            'w-0 min-w-full max-w-full py-4 pl-4',
            visibleButtonCount === 3 && 'pr-28',
            visibleButtonCount === 2 && 'pr-20',
            visibleButtonCount === 1 && 'pr-12',
            visibleButtonCount === 0 && 'pr-4',
            wrapLines ? 'overflow-x-hidden' : 'overflow-x-auto'
          )}
        >
          <pre
            className={cn(
              'm-0 font-mono text-[13px] leading-6 sm:text-xs',
              wrapLines ? 'whitespace-pre-wrap break-all' : 'whitespace-pre'
            )}
          >
            <HighlightedCode
              code={code}
              language={language}
              showLineNumbers={showLineNumbers}
            />
          </pre>
        </div>
        <div className="absolute top-2 right-2 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity duration-200">
          <Button
            size="icon"
            variant="ghost"
            className={cn(
              'size-7 rounded-md',
              'text-muted-foreground/70 hover:text-foreground',
              'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
            )}
            onClick={() => setShowLineNumbers(!showLineNumbers)}
            title={showLineNumbers ? 'Hide line numbers' : 'Show line numbers'}
            aria-label={
              showLineNumbers ? 'Hide line numbers' : 'Show line numbers'
            }
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
                'text-muted-foreground/70 hover:text-foreground',
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
            <Button
              size="icon"
              variant="ghost"
              className={cn(
                'size-7 rounded-md',
                'text-muted-foreground/70 hover:text-foreground',
                'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
              )}
              onClick={handleCopy}
              title={copied ? 'Copied' : 'Copy'}
              aria-label={copied ? 'Copied' : 'Copy'}
            >
              {copied ? (
                <Check className="size-3.5 text-emerald-600 dark:text-emerald-400" />
              ) : (
                <Copy className="size-3.5" />
              )}
            </Button>
          )}
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
  language?: CodeLanguage
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
