// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Check, Globe } from 'lucide-react'
import { CloudflareIcon } from '@/components/icons/DnsProviderIcons'

export type DeliveryProviderChoiceValue = 'none' | 'cloudflare' | 'bunny'

export function DeliveryProviderChoice({
  value,
  onChange,
  cloudflareConfigured,
  bunnyConfigured,
  disabled = false,
}: {
  value: DeliveryProviderChoiceValue
  onChange: (value: DeliveryProviderChoiceValue) => void
  cloudflareConfigured: boolean
  bunnyConfigured: boolean
  disabled?: boolean
}) {
  return (
    <div
      role="group"
      aria-label="Delivery provider"
      className="grid overflow-hidden rounded-lg border bg-card sm:grid-cols-3"
    >
      <button
        type="button"
        aria-pressed={value === 'none'}
        aria-disabled={disabled}
        onClick={() => {
          if (!disabled) onChange('none')
        }}
        className={`min-w-0 border-b p-4 text-left transition-colors last:border-b-0 focus-visible:relative focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring sm:border-b-0 sm:border-r ${value === 'none' ? 'bg-muted/50 ring-1 ring-inset ring-primary' : 'hover:bg-muted/30'} ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}
      >
        <span className="flex min-h-8 items-center justify-between gap-2">
          <Globe className="size-6 shrink-0" aria-hidden="true" />
          {value === 'none' && <Check className="size-4" aria-hidden="true" />}
        </span>
        <span className="mt-3 block font-medium">No CDN default</span>
        <span className="mt-1 block text-xs text-muted-foreground">
          Configure delivery when adding a domain.
        </span>
      </button>
      <button
        type="button"
        aria-pressed={value === 'cloudflare'}
        aria-disabled={
          disabled || (!cloudflareConfigured && value !== 'cloudflare')
        }
        onClick={() => {
          if (!disabled && cloudflareConfigured) onChange('cloudflare')
        }}
        className={`min-w-0 border-b p-4 text-left transition-colors last:border-b-0 focus-visible:relative focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring sm:border-b-0 sm:border-r ${value === 'cloudflare' ? 'bg-muted/50 ring-1 ring-inset ring-primary' : 'hover:bg-muted/30'} ${disabled || (!cloudflareConfigured && value !== 'cloudflare') ? 'cursor-not-allowed opacity-60' : ''}`}
      >
        <span className="flex min-h-8 items-center justify-between gap-2">
          <img
            src="/providers/cloudflare-official.png"
            alt=""
            className="h-7 w-24 object-contain object-left dark:hidden"
          />
          <CloudflareIcon className="hidden size-8 dark:block" />
          {value === 'cloudflare' && (
            <Check className="size-4" aria-hidden="true" />
          )}
        </span>
        <span className="mt-3 block font-medium">Cloudflare</span>
        <span className="mt-1 block text-xs text-muted-foreground">
          {cloudflareConfigured
            ? 'Delivery profile ready. Connect DNS separately to manage records.'
            : 'Create a delivery profile and connect Cloudflare DNS.'}
        </span>
      </button>
      <button
        type="button"
        aria-pressed={value === 'bunny'}
        aria-disabled={disabled || (!bunnyConfigured && value !== 'bunny')}
        onClick={() => {
          if (!disabled && bunnyConfigured) onChange('bunny')
        }}
        className={`min-w-0 p-4 text-left transition-colors focus-visible:relative focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${value === 'bunny' ? 'bg-muted/50 ring-1 ring-inset ring-primary' : 'hover:bg-muted/30'} ${disabled || (!bunnyConfigured && value !== 'bunny') ? 'cursor-not-allowed opacity-60' : ''}`}
      >
        <span className="flex min-h-8 items-center justify-between gap-2">
          <img
            src="/providers/bunny-official.svg"
            alt=""
            className="h-8 w-7 object-contain"
          />
          {value === 'bunny' && <Check className="size-4" aria-hidden="true" />}
        </span>
        <span className="mt-3 block font-medium">bunny.net</span>
        <span className="mt-1 block text-xs text-muted-foreground">
          {bunnyConfigured
            ? 'Validated Pull Zone and delivery profile ready.'
            : 'Needs an active Pull Zone and delivery profile.'}
        </span>
      </button>
    </div>
  )
}
