// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Link } from 'react-router'
import {
  PageContainer,
  PageHeader,
  Status,
  STATUS_TONES,
  type StatusTone,
} from '@temps-sdk/ds'
import rulesRaw from '../../../web/packages/ds/docs/RULES.md?raw'
import brandRaw from '../../../web/packages/ds/docs/brand-guidelines.md?raw'
import handoffRaw from '../../../web/packages/ds/docs/design-system-handoff.md?raw'

/** Splits a markdown doc into `{ heading, body }` sections on `##` headings. */
function sections(markdown: string) {
  const parts = markdown.split(/\n(?=## )/g)
  return parts
    .filter((p) => p.trim().startsWith('## '))
    .map((p) => {
      const [heading, ...rest] = p.split('\n')
      return {
        heading: heading.replace(/^##\s*/, ''),
        body: rest.join('\n').trim(),
      }
    })
}

/** Demos shown next to specific RULES.md sections — keyed by heading text. */
const LIVE_DEMOS: Record<string, ReactNode> = {
  'Status vocabulary': (
    <div className="flex flex-wrap gap-2">
      {(Object.keys(STATUS_TONES) as StatusTone[]).map((tone) => (
        <Status key={tone} tone={tone} />
      ))}
    </div>
  ),
  Tokens: (
    <div
      className="tds flex flex-wrap gap-3 rounded-md border p-3"
      style={{ background: 'var(--background)' }}
    >
      {(
        ['background', 'primary', 'success', 'warning', 'destructive'] as const
      ).map((name) => (
        <div key={name} className="flex flex-col items-center gap-1 text-xs">
          <div
            className="size-10 rounded-md border"
            style={{
              background: `var(--${name})`,
              color: `var(--${name}-foreground, var(--foreground))`,
            }}
          />
          <span style={{ color: 'var(--muted-foreground)' }}>{name}</span>
        </div>
      ))}
    </div>
  ),
}

function Doc({ title, markdown }: { title: string; markdown: string }) {
  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">{title}</h2>
      {sections(markdown).map(({ heading, body }) => (
        <div key={heading} className="grid gap-4 border-t pt-4 md:grid-cols-2">
          <div>
            <h3 className="mb-2 font-medium">{heading}</h3>
            <pre className="whitespace-pre-wrap text-sm text-muted-foreground">
              {body}
            </pre>
          </div>
          {LIVE_DEMOS[heading] ? (
            <div className="rounded-md border bg-muted/20 p-4">
              {LIVE_DEMOS[heading]}
            </div>
          ) : null}
        </div>
      ))}
    </div>
  )
}

/** /guide — renders RULES/brand/handoff content with a live demo beside sections that have one. */
export default function Guide() {
  return (
    <PageContainer>
      <PageHeader
        title="Guide"
        description="RULES.md, brand-guidelines.md, and design-system-handoff.md, rendered from the committed docs so this page can't drift from them."
      />
      <section className="space-y-3 rounded-lg border p-4">
        <h2 className="text-lg font-semibold">Start with the user’s task</h2>
        <p className="text-sm text-muted-foreground">
          Reuse the console’s existing theme. Choose a template for the data,
          then make the status, next action, and recovery path explicit.
        </p>
        <ul className="list-disc space-y-2 pl-5 text-sm">
          <li>
            <Link className="underline underline-offset-4" to="/ledger">
              Compare a collection: Ledger
            </Link>
          </li>
          <li>
            <Link className="underline underline-offset-4" to="/detail">
              Inspect one record: Detail
            </Link>
          </li>
          <li>
            <Link className="underline underline-offset-4" to="/settings">
              Change configuration: Settings
            </Link>
          </li>
          <li>
            <Link className="underline underline-offset-4" to="/table-states">
              Handle loading, empty data, and failures: embedded DataTable
            </Link>
          </li>
          <li>
            <Link className="underline underline-offset-4" to="/onboarding">
              Explain missing setup: PageState
            </Link>
          </li>
        </ul>
        <p className="text-sm text-muted-foreground">
          Before shipping, check that every control works, errors explain how to
          recover, and a shared URL restores filters and tabs. Sample request
          states use invented data; they do not contact a live instance.
        </p>
      </section>
      <Doc title="RULES.md" markdown={rulesRaw} />
      <Doc title="brand-guidelines.md" markdown={brandRaw} />
      <Doc title="design-system-handoff.md" markdown={handoffRaw} />
    </PageContainer>
  )
}
