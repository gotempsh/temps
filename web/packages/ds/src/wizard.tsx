// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Confetti } from '@temps-sdk/ui'
import { cn } from './lib/cn'
import { Check } from 'lucide-react'
import type { ReactNode } from 'react'

export interface WizardStep {
  id: string
  label: string
}

export interface WizardProps {
  title: ReactNode
  description: ReactNode
  currentStep: string
  steps: WizardStep[]
  children: ReactNode
  celebrate?: boolean
  fullWidth?: boolean
  headerActions?: ReactNode
}

/**
 * Step-indicator + title/description shell for any multi-step flow (setup
 * wizards, onboarding, "connect a resource" flows) — not a form primitive;
 * `Settings`/`Field` still own validated input collection inside each step.
 * Promoted as-is from
 * `web/src/components/project/setup/SetupWizardShell.tsx` (unchanged
 * behavior — that file now re-exports this as `SetupWizardShell`, plus its
 * own setup-specific `WizardStepId` union, so its 4 existing consumers keep
 * working unchanged). `Confetti` is already re-exported via `@temps-sdk/ui`
 * (`web/src/components/ui/confetti.tsx`), so it's imported from there
 * rather than duplicated.
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
}: WizardProps) {
  const currentIndex = steps.findIndex((step) => step.id === currentStep)

  return (
    <div className={cn('w-full', fullWidth ? 'space-y-6' : 'space-y-8 py-4')}>
      <Confetti active={celebrate} duration={2500} particleCount={80} />

      <div className={cn(fullWidth && 'flex items-start justify-between gap-4')}>
        <div className={cn('space-y-2', !fullWidth && 'text-center')}>
          <h1 className="text-2xl font-semibold tracking-tight text-balance">{title}</h1>
          <p className="text-sm text-muted-foreground text-pretty">{description}</p>
        </div>
        {headerActions}
      </div>

      <ol
        role="list"
        aria-label="Setup progress"
        className={cn('flex items-center gap-2 sm:gap-4', !fullWidth && 'justify-center')}
      >
        {steps.map((step, index) => {
          const stepIndex = index
          const isDone = stepIndex < currentIndex
          const isActive = step.id === currentStep
          const isLast = index === steps.length - 1
          return (
            <li
              key={step.id}
              aria-current={isActive ? 'step' : undefined}
              aria-label={`${step.label}${isDone ? ', completed' : ''}`}
              className={cn('flex min-w-0 items-center gap-2 sm:gap-4', fullWidth && !isLast && 'flex-1')}
            >
              <div className="flex items-center gap-2">
                <span
                  className={cn(
                    'flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums transition-colors',
                    isDone && 'border-emerald-500 bg-emerald-500 text-white',
                    isActive && !isDone && 'border-primary bg-primary text-primary-foreground',
                    !isDone && !isActive && 'border-muted-foreground/30 text-muted-foreground',
                  )}
                >
                  {isDone ? <Check className="size-4" strokeWidth={3} /> : index + 1}
                </span>
                <span
                  className={cn(
                    'hidden text-sm font-medium',
                    fullWidth ? 'lg:inline' : 'sm:inline',
                    isDone || isActive ? 'text-foreground' : 'text-muted-foreground',
                  )}
                >
                  {step.label}
                </span>
              </div>
              {!isLast && (
                <span
                  aria-hidden
                  className={cn(
                    'h-px',
                    fullWidth ? 'min-w-2 flex-1' : 'w-8 sm:w-12',
                    isDone ? 'bg-emerald-500' : 'bg-border',
                  )}
                />
              )}
            </li>
          )
        })}
      </ol>

      <div>{children}</div>
    </div>
  )
}
