// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Confetti } from '@/components/ui/confetti'
import { cn } from '@/lib/utils'
import { Check } from 'lucide-react'
import { ReactNode } from 'react'

export type WizardStepId = 'framework' | 'install' | 'waiting'

interface WizardStep {
  id: string
  label: string
}

interface SetupWizardShellProps {
  title: string
  description: string
  currentStep: string
  steps: WizardStep[]
  children: ReactNode
  celebrate?: boolean
  fullWidth?: boolean
  headerActions?: ReactNode
}

export function SetupWizardShell({
  title,
  description,
  currentStep,
  steps,
  children,
  celebrate = false,
  fullWidth = false,
  headerActions,
}: SetupWizardShellProps) {
  const currentIndex = steps.findIndex((step) => step.id === currentStep)

  return (
    <div className={cn('w-full', fullWidth ? 'space-y-6' : 'space-y-8 py-4')}>
      <Confetti active={celebrate} duration={2500} particleCount={80} />

      <div
        className={cn(fullWidth && 'flex items-start justify-between gap-4')}
      >
        <div className={cn('space-y-2', !fullWidth && 'text-center')}>
          <h1 className="text-2xl font-semibold tracking-tight text-balance">
            {title}
          </h1>
          <p className="text-sm text-muted-foreground text-pretty">
            {description}
          </p>
        </div>
        {headerActions}
      </div>

      <ol
        role="list"
        aria-label="Setup progress"
        className={cn(
          'flex items-center gap-2 sm:gap-4',
          !fullWidth && 'justify-center'
        )}
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
              className={cn(
                'flex min-w-0 items-center gap-2 sm:gap-4',
                fullWidth && !isLast && 'flex-1'
              )}
            >
              <div className="flex items-center gap-2">
                <span
                  className={cn(
                    'flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums transition-colors',
                    isDone && 'border-emerald-500 bg-emerald-500 text-white',
                    isActive &&
                      !isDone &&
                      'border-primary bg-primary text-primary-foreground',
                    !isDone &&
                      !isActive &&
                      'border-muted-foreground/30 text-muted-foreground'
                  )}
                >
                  {isDone ? (
                    <Check className="size-4" strokeWidth={3} />
                  ) : (
                    index + 1
                  )}
                </span>
                <span
                  className={cn(
                    'hidden text-sm font-medium',
                    fullWidth ? 'lg:inline' : 'sm:inline',
                    isDone || isActive
                      ? 'text-foreground'
                      : 'text-muted-foreground'
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
                    isDone ? 'bg-emerald-500' : 'bg-border'
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
