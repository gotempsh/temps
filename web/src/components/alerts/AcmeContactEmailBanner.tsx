// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { ShieldAlert } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { Link, useLocation } from 'react-router'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import {
  CERTIFICATE_SETTINGS_PATH,
  contactEmailSchema,
  needsAcmeContactEmail,
  type ContactEmailFormData,
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
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<ContactEmailFormData>({
    resolver: zodResolver(contactEmailSchema),
    defaultValues: { email: '' },
  })

  // The settings page has its own inline warning on the same field.
  if (pathname === CERTIFICATE_SETTINGS_PATH || !settings) return null
  if (!needsAcmeContactEmail(settings)) return null

  const onSubmit = async ({ email }: ContactEmailFormData) => {
    try {
      await updateSettings.mutateAsync({
        letsencrypt: { ...settings.letsencrypt, email },
      })
      toast.success('Contact email saved', {
        description:
          'Certificates for environment URLs without a wildcard are requested automatically from now on.',
      })
    } catch {
      // `useUpdateSettings` already raised the error toast; the banner stays
      // so the email can be corrected and sent again.
    }
  }

  const saving = isSubmitting || updateSettings.isPending
  const emailError = errors.email?.message

  return (
    <form
      onSubmit={handleSubmit(onSubmit)}
      noValidate
      role="alert"
      className="flex flex-col gap-2 border-b border-amber-200 bg-amber-50/70 px-4 py-2 text-sm text-amber-950 sm:flex-row sm:flex-wrap sm:items-center dark:border-amber-900/60 dark:bg-amber-950/20 dark:text-amber-100"
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
          aria-invalid={emailError ? true : undefined}
          aria-describedby={emailError ? 'acme-contact-email-error' : undefined}
          placeholder="ops@your-domain.com"
          disabled={saving}
          className="h-8 min-w-0 flex-1 bg-background sm:w-64 sm:flex-none"
          {...register('email')}
        />
        <Button type="submit" size="sm" disabled={saving}>
          {saving ? 'Saving…' : 'Save email'}
        </Button>
      </div>
      {emailError && (
        <p
          id="acme-contact-email-error"
          className="text-xs text-destructive sm:basis-full"
        >
          {emailError}
        </p>
      )}
    </form>
  )
}
