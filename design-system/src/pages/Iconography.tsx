// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ArrowLeft,
  ArrowRight,
  CalendarDays,
  Check,
  Copy,
  GitBranch,
  Plus,
  Search,
  Trash2,
} from 'lucide-react'
import { Link } from 'react-router'
import {
  GitProviderMark,
  PageContainer,
  PageHeader,
  Status,
  STATUS_TONES,
  type StatusTone,
} from '@temps-sdk/ds'

const actions = [
  { icon: ArrowLeft, label: 'Back', meaning: 'Return to the previous step' },
  { icon: ArrowRight, label: 'Continue', meaning: 'Advance to the next step' },
  { icon: Plus, label: 'Create', meaning: 'Add a new resource' },
  { icon: Search, label: 'Search', meaning: 'Find or filter records' },
  {
    icon: CalendarDays,
    label: 'Time range',
    meaning: 'Choose dates and times',
  },
  { icon: Copy, label: 'Copy', meaning: 'Copy a value to the clipboard' },
  { icon: Trash2, label: 'Delete', meaning: 'Remove a named resource' },
  { icon: GitBranch, label: 'Branch', meaning: 'Identify a repository branch' },
]

export default function Iconography() {
  return (
    <PageContainer>
      <PageHeader
        title="Iconography"
        description="Recognizable identities, consistent actions, and explicit states."
      />
      <section className="space-y-4 rounded-lg border p-5">
        <div>
          <h2 className="text-lg font-semibold">Provider identity</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Reuse the console’s provider marks in the current text color. Always
            show the provider name beside a selection.
          </p>
        </div>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {['GitHub', 'GitLab', 'Bitbucket', 'Gitea'].map((provider) => (
            <div
              key={provider}
              className="flex items-center gap-3 rounded-md border p-4"
            >
              <GitProviderMark provider={provider} />
              <span className="text-sm font-medium">{provider}</span>
            </div>
          ))}
        </div>
        <Link
          className="inline-flex items-center gap-2 text-sm underline underline-offset-4"
          to="/wizard"
        >
          See provider selection in the wizard{' '}
          <ArrowRight aria-hidden="true" className="size-4" />
        </Link>
      </section>
      <section className="space-y-4 rounded-lg border p-5">
        <div>
          <h2 className="text-lg font-semibold">Interface actions</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Lucide icons use a consistent stroke and size. These are reference
            samples, not interactive controls.
          </p>
        </div>
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {actions.map(({ icon: Icon, label, meaning }) => (
            <div key={label} className="flex items-start gap-3">
              <Icon aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
              <div>
                <p className="text-sm font-medium">{label}</p>
                <p className="mt-1 text-xs text-muted-foreground">{meaning}</p>
              </div>
            </div>
          ))}
        </div>
      </section>
      <section className="space-y-4 rounded-lg border p-5">
        <div>
          <h2 className="text-lg font-semibold">Status and progress</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Outcomes use the shared status vocabulary: a symbol, color, and a
            readable word. Progress and selection stay neutral.
          </p>
        </div>
        <div className="flex flex-wrap gap-3">
          {(Object.keys(STATUS_TONES) as StatusTone[]).map((tone) => (
            <Status key={tone} tone={tone} />
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-6 border-t pt-4 text-sm">
          <span className="inline-flex items-center gap-2">
            <span
              aria-hidden="true"
              className="flex size-7 items-center justify-center rounded-full border bg-primary text-xs text-primary-foreground"
            >
              2
            </span>
            Current step
          </span>
          <span className="inline-flex items-center gap-2">
            <Check aria-hidden="true" className="size-4" />
            Completed step
          </span>
        </div>
      </section>
      <p className="text-sm text-muted-foreground">
        Use visible action labels where space allows. If a button contains only
        an icon, its accessible name must say what it does. Decorative icons
        beside text should not be announced twice.
      </p>
    </PageContainer>
  )
}
