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
  description: ReactNode
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
        !fullWidth && 'py-4',
        footer && !fullWidth && 'mx-auto max-w-3xl'
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

      <ol
        aria-label="Setup progress"
        className="flex gap-3 rounded-lg border bg-muted/20 p-4 sm:gap-6"
      >
        {steps.map((step, index) => {
          const isDone = index < currentIndex
          const isActive = step.id === currentStep
          return (
            <li
              key={step.id}
              aria-current={isActive ? 'step' : undefined}
              aria-label={`${step.label}${isDone ? ', completed' : ''}`}
              className="flex min-w-0 flex-1 flex-col items-start gap-2 sm:flex-row sm:gap-3"
            >
              <span
                aria-hidden="true"
                className={cn(
                  'flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums',
                  isActive &&
                    'border-primary bg-primary text-primary-foreground',
                  isDone && 'border-foreground bg-background text-foreground',
                  !isDone &&
                    !isActive &&
                    'border-border bg-background text-muted-foreground'
                )}
              >
                {isDone ? <Check className="size-4" /> : index + 1}
              </span>
              <div className="min-w-0 pt-0.5">
                <span
                  className={cn(
                    'block text-sm font-medium',
                    isDone || isActive
                      ? 'text-foreground'
                      : 'text-muted-foreground'
                  )}
                >
                  {step.label}
                </span>
                {step.description && (
                  <span className="mt-1 hidden text-xs text-muted-foreground sm:block">
                    {step.description}
                  </span>
                )}
              </div>
            </li>
          )
        })}
      </ol>

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
