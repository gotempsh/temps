// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Confetti } from '@/components/ui/confetti'
import { cn } from '@/lib/utils'
import { Check } from 'lucide-react'
import { ReactNode } from 'react'

export type WizardStepId = 'framework' | 'install' | 'waiting'

interface WizardStep {
  id: WizardStepId
  label: string
}

interface SetupWizardShellProps {
  title: string
  description: string
  currentStep: WizardStepId
  steps: WizardStep[]
  children: ReactNode
  celebrate?: boolean
}

const STEP_ORDER: WizardStepId[] = ['framework', 'install', 'waiting']

export function SetupWizardShell({
  title,
  description,
  currentStep,
  steps,
  children,
  celebrate = false,
}: SetupWizardShellProps) {
  const currentIndex = STEP_ORDER.indexOf(currentStep)

  return (
    <div className="mx-auto max-w-3xl space-y-8 py-4">
      <Confetti active={celebrate} duration={2500} particleCount={80} />

      <div className="space-y-2 text-center">
        <h1 className="text-2xl font-semibold tracking-tight text-balance">
          {title}
        </h1>
        <p className="text-sm text-muted-foreground text-pretty">
          {description}
        </p>
      </div>

      <ol
        role="list"
        className="flex items-center justify-center gap-2 sm:gap-4"
      >
        {steps.map((step, index) => {
          const stepIndex = STEP_ORDER.indexOf(step.id)
          const isDone = stepIndex < currentIndex
          const isActive = step.id === currentStep
          const isLast = index === steps.length - 1
          return (
            <li key={step.id} className="flex items-center gap-2 sm:gap-4">
              <div className="flex items-center gap-2">
                <span
                  className={cn(
                    'flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums transition-colors',
                    isDone &&
                      'border-emerald-500 bg-emerald-500 text-white',
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
                    'hidden text-sm font-medium sm:inline',
                    (isDone || isActive)
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
                    'h-px w-8 sm:w-12',
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
