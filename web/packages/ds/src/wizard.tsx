// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Confetti } from '@temps-sdk/ui'
import { cn } from './lib/cn'
import { Check } from 'lucide-react'
import type { ReactNode } from 'react'
import { PageHeader } from './page-header'

export interface WizardStep {
  id: string
  label: string
  description?: string
}

export interface WizardProps {
  title: ReactNode
  description?: ReactNode
  currentStep: string
  steps: WizardStep[]
  children: ReactNode
  celebrate?: boolean
  /** Expand a standalone step surface when its content needs more room. */
  fullWidth?: boolean
  headerActions?: ReactNode
  /** Adds a bordered step surface with a persistent, separate action footer. */
  footer?: ReactNode
}

/**
 * Shared title, progress and optional step surface for multi-step flows.
 * Promoted from SetupWizardShell, which remains a thin re-export. Callers
 * own step validation, navigation and PageContainer; Field owns input labels.
 * Existing consumers without a footer keep their own content surfaces.
 */
export function Wizard({
  title,
  description,
  currentStep,
  steps,
  children,
  celebrate = false,
  fullWidth = false,
  headerActions,
  footer,
}: WizardProps) {
  const currentIndex = steps.findIndex((step) => step.id === currentStep)

  return (
    <div
      className={cn(
        'w-full min-w-0 space-y-6',
        !fullWidth && 'mx-auto max-w-5xl py-4'
      )}
    >
      <div className="motion-reduce:hidden">
        <Confetti active={celebrate} duration={2500} particleCount={80} />
      </div>
      <PageHeader
        title={title}
        description={description}
        actions={headerActions}
      />

      {/* Unboxed: the step surface below is the page's one surface. In a
          narrow column (a phone, or a tablet beside the sidebar) only the
          current step is named; screen readers still get every step through
          each item's label. */}
      <div className="@container">
        <ol
          aria-label="Setup progress"
          className="flex items-center gap-2 sm:gap-3"
        >
          {steps.map((step, index) => {
            const isDone = index < currentIndex
            const isActive = step.id === currentStep
            const isLast = index === steps.length - 1
            return (
              <li
                key={step.id}
                aria-current={isActive ? 'step' : undefined}
                aria-label={`${step.label}${isDone ? ', completed' : ''}`}
                className={cn(
                  'flex min-w-0 items-center gap-2 sm:gap-3',
                  !isLast && 'flex-1',
                  // The only label shown on a phone takes the room it needs.
                  isActive && '@max-3xl:shrink-0 @max-3xl:basis-auto'
                )}
              >
                <span
                  aria-hidden="true"
                  className={cn(
                    'flex size-6 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums',
                    isActive &&
                      'border-primary bg-primary text-primary-foreground',
                    isDone && 'border-foreground bg-background text-foreground',
                    !isDone &&
                      !isActive &&
                      'border-border bg-background text-muted-foreground'
                  )}
                >
                  {isDone ? <Check className="size-3.5" /> : index + 1}
                </span>
                <span
                  className={cn('min-w-0', !isActive && '@max-3xl:sr-only')}
                >
                  <span
                    className={cn(
                      'block truncate text-sm font-medium',
                      isDone || isActive
                        ? 'text-foreground'
                        : 'text-muted-foreground'
                    )}
                  >
                    {step.label}
                  </span>
                  {step.description && (
                    <span className="hidden truncate text-xs text-muted-foreground sm:block">
                      {step.description}
                    </span>
                  )}
                </span>
                {!isLast && (
                  <span
                    aria-hidden="true"
                    className={cn(
                      'h-px min-w-3 flex-1',
                      isDone ? 'bg-foreground/60' : 'bg-border'
                    )}
                  />
                )}
              </li>
            )
          })}
        </ol>
      </div>

      {footer ? (
        <section className="min-w-0 overflow-hidden rounded-lg border bg-card text-card-foreground">
          <div className="p-4 sm:p-6">{children}</div>
          <div className="flex flex-col gap-3 border-t bg-muted/20 p-4 sm:flex-row sm:items-center sm:justify-start sm:px-6">
            {footer}
          </div>
        </section>
      ) : (
        <div>{children}</div>
      )}
    </div>
  )
}
