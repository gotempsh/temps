// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, type ReactNode } from 'react'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@temps-sdk/ds'
import { Check } from 'lucide-react'
import { CodeBlock, type CodeLanguage } from '@/components/ui/code-block'
import { cn } from '@/lib/utils'

export interface SetupGuideStep {
  title: string
  caption: ReactNode
  /** Marks a step the user has already completed. */
  done?: boolean
  /** The code, command or action that does the step. */
  content?: ReactNode
}

/**
 * How to start using a feature, as numbered steps each with the code or
 * action that does it. It is the empty state of a list or page, so it is one
 * bordered surface rather than a stack of cards.
 */
export function SetupGuide({
  title,
  description,
  action,
  steps,
}: {
  title: string
  /** One line on why the page is empty, shown under the title. */
  description?: ReactNode
  /** One action for the whole guide, beside the title (a "Copy prompt"). */
  action?: ReactNode
  steps: SetupGuideStep[]
}) {
  const titleId = useId()
  return (
    <section
      aria-labelledby={titleId}
      className="rounded-lg border bg-card text-card-foreground"
    >
      <div className="flex flex-wrap items-start justify-between gap-3 border-b px-4 py-3">
        <div className="min-w-0 space-y-0.5">
          <h2 id={titleId} className="text-base font-medium">
            {title}
          </h2>
          {description && (
            <p className="text-sm text-muted-foreground">{description}</p>
          )}
        </div>
        {action}
      </div>
      <ol className="divide-y">
        {steps.map((step, index) => (
          <li key={step.title} className="flex gap-3 px-4 py-4">
            <span
              aria-hidden
              className={cn(
                'flex size-6 shrink-0 items-center justify-center rounded-full border text-xs font-medium',
                step.done &&
                  'border-transparent bg-primary text-primary-foreground'
              )}
            >
              {step.done ? <Check className="size-3.5" /> : index + 1}
            </span>
            <div className="min-w-0 flex-1 space-y-3">
              <div className="space-y-0.5">
                <h3 className="text-sm font-medium">
                  {step.title}
                  {step.done && <span className="sr-only"> (done)</span>}
                </h3>
                <p className="text-sm text-muted-foreground [overflow-wrap:anywhere]">
                  {step.caption}
                </p>
              </div>
              {step.content}
            </div>
          </li>
        ))}
      </ol>
    </section>
  )
}

export interface SetupSnippet {
  value: string
  label: string
  /** Shown before the code, for an install command or a file name. */
  install?: string
  code: string
  language: CodeLanguage
}

/** One snippet per stack, picked with tabs; the first stack opens. */
export function SnippetTabs({ snippets }: { snippets: SetupSnippet[] }) {
  return (
    <Tabs defaultValue={snippets[0]?.value} className="w-full">
      <TabsList>
        {snippets.map((snippet) => (
          <TabsTrigger key={snippet.value} value={snippet.value}>
            {snippet.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {snippets.map((snippet) => (
        <TabsContent
          key={snippet.value}
          value={snippet.value}
          className="mt-3 space-y-3"
        >
          {snippet.install && (
            <CodeBlock code={snippet.install} language="bash" />
          )}
          <CodeBlock code={snippet.code} language={snippet.language} />
        </TabsContent>
      ))}
    </Tabs>
  )
}
