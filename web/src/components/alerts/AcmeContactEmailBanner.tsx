// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type FormEvent } from 'react'
import { ShieldAlert } from 'lucide-react'
import { Link, useLocation } from 'react-router'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import {
  CERTIFICATE_SETTINGS_PATH,
  isValidContactEmail,
  needsAcmeContactEmail,
} from '@/lib/on-demand-tls'

/**
 * App-wide alert shown while on-demand HTTPS is on and would run, but no
 * Let's Encrypt contact email is set: every certificate attempt fails until
 * it is. The field is inline so fixing it is one step, not a trip through
 * settings. There is deliberately no dismiss action -- nothing is issued and
 * nothing renews until the email exists, so hiding it would hide a real
 * outage of HTTPS for new environment URLs.
 *
 * Only an instance administrator ever sees it: `useSettings` never sends the
 * read for other roles, so for them `settings` stays undefined and this
 * renders nothing.
 */
export function AcmeContactEmailBanner() {
  const { pathname } = useLocation()
  const { data: settings } = useSettings()
  const updateSettings = useUpdateSettings()
  const [email, setEmail] = useState('')
  const [touched, setTouched] = useState(false)

  // The settings page has its own inline warning on the same field.
  if (pathname === CERTIFICATE_SETTINGS_PATH || !settings) return null
  if (!needsAcmeContactEmail(settings)) return null

  const invalid = touched && !isValidContactEmail(email)

  const onSubmit = (event: FormEvent) => {
    event.preventDefault()
    setTouched(true)
    if (!isValidContactEmail(email)) return
    updateSettings.mutate(
      {
        letsencrypt: {
          ...settings.letsencrypt,
          email: email.trim(),
        },
      },
      {
        onSuccess: () => {
          toast.success('Contact email saved', {
            description:
              'Certificates for environment URLs without a wildcard are now requested automatically.',
          })
        },
      }
    )
  }

  return (
    <form
      onSubmit={onSubmit}
      role="alert"
      className="flex flex-col gap-2 border-b border-amber-200 bg-amber-50/70 px-4 py-2 text-sm text-amber-950 sm:flex-row sm:items-center dark:border-amber-900/60 dark:bg-amber-950/20 dark:text-amber-100"
    >
      <div className="flex min-w-0 flex-1 items-start gap-2">
        <ShieldAlert className="mt-0.5 size-4 shrink-0 text-amber-700 dark:text-amber-300" />
        <p className="min-w-0">
          <span className="font-medium">
            Set a Let&apos;s Encrypt contact email to get HTTPS
          </span>
          <span className="text-amber-900/90 dark:text-amber-100/90">
            {' '}
            — environment URLs without a wildcard certificate cannot be issued
            or renewed until it is set.{' '}
            <Link
              to={CERTIFICATE_SETTINGS_PATH}
              className="font-medium underline-offset-2 hover:underline"
            >
              Open settings
            </Link>
          </span>
        </p>
      </div>
      <div className="flex w-full items-center gap-2 sm:w-auto">
        <Input
          type="email"
          inputMode="email"
          autoComplete="email"
          aria-label="Let's Encrypt contact email"
          aria-invalid={invalid}
          placeholder="ops@your-domain.com"
          value={email}
          disabled={updateSettings.isPending}
          onChange={(event) => setEmail(event.target.value)}
          onBlur={() => setTouched(email.length > 0)}
          className="h-8 min-w-0 flex-1 bg-background sm:w-64 sm:flex-none"
        />
        <Button
          type="submit"
          size="sm"
          disabled={updateSettings.isPending || email.trim().length === 0}
        >
          {updateSettings.isPending ? 'Saving…' : 'Save email'}
        </Button>
      </div>
      {invalid && (
        <p className="text-xs text-destructive sm:basis-full">
          Enter a valid email address, such as ops@your-domain.com.
        </p>
      )}
    </form>
  )
}
