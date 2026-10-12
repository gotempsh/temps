// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AiProviderIcon } from '@/lib/ai-providers'
import { cn } from '@/lib/utils'

export interface ProviderPickerOption {
  id: string
  name: string
  tagline: string
}

/**
 * Provider choice for the add-key dialog, shown when the dialog was opened
 * without a preselected provider. Buttons rather than a dropdown so every
 * supported provider and what it is for is visible without opening anything.
 */
export function ProviderKeyProviderPicker({
  providers,
  value,
  onChange,
  disabled = false,
  invalid = false,
}: {
  providers: readonly ProviderPickerOption[]
  value: string
  onChange: (providerId: string) => void
  disabled?: boolean
  invalid?: boolean
}) {
  return (
    <div
      role="radiogroup"
      aria-label="Provider"
      aria-invalid={invalid || undefined}
      className="grid gap-2 sm:grid-cols-2"
    >
      {providers.map((provider) => {
        const selected = provider.id === value
        return (
          <button
            key={provider.id}
            type="button"
            role="radio"
            aria-checked={selected}
            disabled={disabled}
            data-provider={provider.id}
            onClick={() => onChange(provider.id)}
            className={cn(
              'flex min-w-0 items-center gap-3 rounded-md border px-3 py-2.5 text-left transition-colors',
              'hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              'disabled:pointer-events-none disabled:opacity-50',
              selected && 'border-primary bg-muted/40',
              invalid && !selected && 'border-destructive'
            )}
          >
            <AiProviderIcon provider={provider.id} size={32} />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium">
                {provider.name}
              </span>
              <span className="block truncate text-xs text-muted-foreground">
                {provider.tagline}
              </span>
            </span>
          </button>
        )
      })}
    </div>
  )
}
