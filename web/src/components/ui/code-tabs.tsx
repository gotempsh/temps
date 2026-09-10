// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useMemo, type ReactNode } from 'react'
import { CodeBlock } from '@/components/ui/code-block'
import { cn } from '@/lib/utils'

export type CodeExampleLanguage =
  | 'bash'
  | 'shell'
  | 'yaml'
  | 'json'
  | 'javascript'
  | 'typescript'
  | 'python'
  | 'go'
  | 'text'

export type CodeExample = {
  /** Unique id used for tab state. Also used as the React key. */
  id: string
  /** Tab label shown to the user (e.g. "cURL", "Python", "Node.js"). */
  label: string
  /** Language passed to the underlying CodeBlock for syntax highlighting. */
  language: CodeExampleLanguage
  /** Source code to render. */
  code: string
}

type CodeTabsProps = {
  examples: CodeExample[]
  /** Id of the example to show initially. Defaults to the first example. */
  defaultExampleId?: string
  /** Controlled selected id. Uncontrolled if omitted. */
  value?: string
  /** Called when the user switches tabs. */
  onValueChange?: (id: string) => void
  /**
   * Optional content rendered on the right side of the tab bar
   * (e.g. a provider dropdown). Rendered as-is.
   */
  rightSlot?: ReactNode
  /**
   * When true, renders on a dark zinc chrome regardless of the active theme
   * (used for marketing-style landing panels). The inner code inherits a
   * light foreground so unstyled tokens stay visible against the dark bg.
   */
  dark?: boolean
  /** Show line numbers in the code block. Defaults to true. */
  showLineNumbers?: boolean
  className?: string
}

/**
 * Reusable multi-language code example tabs.
 *
 * Typical shape: a row of language tabs on the left, an optional context
 * switcher on the right (e.g. a provider <Select>), and a <CodeBlock>
 * underneath. Wraps the existing CodeBlock so syntax highlighting is
 * consistent across the app.
 *
 * `dark` forces a dark chrome for marketing-style callouts. Without it,
 * the component follows the user's theme.
 */
export function CodeTabs({
  examples,
  defaultExampleId,
  value,
  onValueChange,
  rightSlot,
  dark = false,
  showLineNumbers = true,
  className,
}: CodeTabsProps) {
  const fallbackId = examples[0]?.id ?? ''
  const [internalId, setInternalId] = useState(defaultExampleId ?? fallbackId)
  const activeId = value ?? internalId
  const active = useMemo(
    () => examples.find((e) => e.id === activeId) ?? examples[0],
    [examples, activeId],
  )

  const handleSelect = (id: string) => {
    if (value === undefined) setInternalId(id)
    onValueChange?.(id)
  }

  if (!active) return null

  return (
    <div
      className={cn(
        'flex flex-col overflow-hidden rounded-lg border',
        dark
          ? 'border-white/10 bg-zinc-950 text-zinc-100'
          : 'bg-card',
        className,
      )}
    >
      <div
        className={cn(
          'flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2',
          dark ? 'border-white/10' : 'border-border',
        )}
      >
        <div className="flex items-center gap-0.5">
          {examples.map((example) => {
            const isActive = example.id === active.id
            return (
              <button
                key={example.id}
                type="button"
                onClick={() => handleSelect(example.id)}
                className={cn(
                  'rounded-md px-2.5 py-1 text-xs font-medium transition-colors',
                  dark
                    ? isActive
                      ? 'bg-white/10 text-white'
                      : 'text-zinc-400 hover:text-zinc-100'
                    : isActive
                      ? 'bg-muted text-foreground'
                      : 'text-muted-foreground hover:text-foreground',
                )}
              >
                {example.label}
              </button>
            )
          })}
        </div>
        {rightSlot ? <div className="flex items-center">{rightSlot}</div> : null}
      </div>
      <div
        className={cn(
          'flex-1 p-4',
          // In dark chrome mode we force a light foreground so tokens
          // without an explicit color utility don't inherit the theme's
          // dark `text-foreground` and disappear against zinc-950.
          dark && '[&_code]:!text-zinc-100',
        )}
      >
        <CodeBlock
          code={active.code}
          language={active.language}
          defaultShowLineNumbers={showLineNumbers}
          className={cn(
            '[&_pre]:!text-[13px]',
            dark &&
              '[&>div]:!border-0 [&>div]:!bg-transparent [&>div]:hover:!bg-transparent',
          )}
        />
      </div>
    </div>
  )
}
