// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { LucideIcon } from 'lucide-react'
import { useId } from 'react'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { cn } from '@/lib/utils'

export interface Choice<T extends string> {
  value: T
  label: string
  description: string
  icon?: LucideIcon
}

/**
 * One choice out of a few, each with a sentence saying what it does: a single
 * bordered list of radio rows. The saved value is not marked here; the card's
 * Save (disabled until the choice changes) already tells the two apart.
 */
export function ChoiceList<T extends string>({
  label,
  value,
  onValueChange,
  choices,
  disabled,
}: {
  /** Accessible name of the group, usually the card title. */
  label: string
  value: T
  onValueChange: (value: T) => void
  choices: Choice<T>[]
  disabled?: boolean
}) {
  const id = useId()
  return (
    <RadioGroup
      aria-label={label}
      value={value}
      onValueChange={(next) => onValueChange(next as T)}
      disabled={disabled}
      className="gap-0 divide-y overflow-hidden rounded-md border"
    >
      {choices.map((choice) => {
        const itemId = `${id}-${choice.value}`
        const Icon = choice.icon
        return (
          <label
            key={choice.value}
            htmlFor={itemId}
            className={cn(
              'flex cursor-pointer items-start gap-3 px-4 py-3 transition-colors hover:bg-muted/40',
              value === choice.value && 'bg-muted/40'
            )}
          >
            <RadioGroupItem
              id={itemId}
              value={choice.value}
              aria-labelledby={`${itemId}-label`}
              aria-describedby={`${itemId}-description`}
              className="mt-0.5 shrink-0"
            />
            <span className="min-w-0 flex-1 space-y-0.5">
              <span
                id={`${itemId}-label`}
                className="flex items-center gap-2 text-sm font-medium"
              >
                {Icon && (
                  <Icon
                    className="size-4 shrink-0 text-muted-foreground"
                    aria-hidden="true"
                  />
                )}
                {choice.label}
              </span>
              <span
                id={`${itemId}-description`}
                className="block text-sm text-muted-foreground"
              >
                {choice.description}
              </span>
            </span>
          </label>
        )
      })}
    </RadioGroup>
  )
}
