// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { AlertCircle, RefreshCw, Save } from 'lucide-react'
import { useEffect } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

import { Button, Callout, PageHeader, SettingsGroup } from '@temps-sdk/ds'
import type { OnDemandTlsSettings } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import {
  DEPLOYMENT_URL_MODES,
  HOURLY_CAP_RANGE,
  MAX_CONCURRENT_RANGE,
  effectiveZone,
  isValidZoneInput,
  normalizeZoneInput,
  onDemandTlsBlockers,
  type OnDemandTlsSettingsSource,
} from '@/lib/on-demand-tls'

const formSchema = z.object({
  enabled: z.boolean(),
  zone: z
    .string()
    .refine(
      isValidZoneInput,
      'Enter a bare domain such as apps.example.com, without a scheme, port, path or wildcard'
    ),
  max_concurrent: z
    .number({ error: 'Enter a number' })
    .int()
    .min(MAX_CONCURRENT_RANGE.min)
    .max(MAX_CONCURRENT_RANGE.max),
  hourly_cap: z
    .number({ error: 'Enter a number' })
    .int()
    .min(HOURLY_CAP_RANGE.min)
    .max(HOURLY_CAP_RANGE.max),
  deployment_url_mode: z.enum(DEPLOYMENT_URL_MODES),
})

type FormData = z.infer<typeof formSchema>

const DEFAULTS: FormData = {
  enabled: false,
  zone: '',
  max_concurrent: 3,
  hourly_cap: 10,
  deployment_url_mode: 'http',
}

const MODE_OPTIONS: Array<{
  value: FormData['deployment_url_mode']
  title: string
  description: string
}> = [
  {
    value: 'http',
    title: 'Serve over HTTP',
    description:
      'Per-deployment URLs answer on plain HTTP. They never get a certificate of their own.',
  },
  {
    value: 'redirect_to_env',
    title: 'Redirect to the environment URL',
    description:
      'Per-deployment URLs redirect (308) to the stable environment URL, which does get a certificate.',
  },
]

function toFormData(
  settings: { on_demand_tls?: OnDemandTlsSettings } | undefined
): FormData {
  const stored = settings?.on_demand_tls
  if (!stored) return DEFAULTS
  const mode = DEPLOYMENT_URL_MODES.find(
    (m) => m === stored.deployment_url_mode
  )
  return {
    enabled: stored.enabled ?? DEFAULTS.enabled,
    zone: stored.zone ?? '',
    max_concurrent: stored.max_concurrent ?? DEFAULTS.max_concurrent,
    hourly_cap: stored.hourly_cap ?? DEFAULTS.hourly_cap,
    deployment_url_mode: mode ?? DEFAULTS.deployment_url_mode,
  }
}

/**
 * On-demand (lazy) HTTP-01 certificate issuance, ADR-018. The proxy reads the
 * switch, zone and limits once at startup, so this page says so instead of
 * implying a save takes effect immediately; only the deployment URL mode is
 * read per request.
 */
export function OnDemandTlsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error, refetch } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    control,
    handleSubmit,
    formState: { isDirty, isSubmitting, errors },
    reset,
  } = useForm<FormData>({
    resolver: zodResolver(formSchema),
    // Seed from cached settings so a revisit renders the stored values on the
    // first paint; the effect below re-seeds when a fetch completes.
    defaultValues: toFormData(settings),
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'On-demand TLS' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('On-demand TLS')

  useEffect(() => {
    if (settings) reset(toFormData(settings))
  }, [settings, reset])

  const enabled = useWatch({ control, name: 'enabled' })
  const zone = useWatch({ control, name: 'zone' })

  const onSubmit = async (data: FormData) => {
    try {
      await updateSettings.mutateAsync({
        on_demand_tls: {
          enabled: data.enabled,
          zone: normalizeZoneInput(data.zone),
          max_concurrent: data.max_concurrent,
          hourly_cap: data.hourly_cap,
          deployment_url_mode: data.deployment_url_mode,
        },
      })
      reset(data)
      toast.success('On-demand TLS saved', {
        description:
          'Restart Temps to apply the switch, zone and limits. The deployment URL mode applies right away.',
      })
    } catch {
      // useUpdateSettings already reports the server's reason in a toast.
    }
  }

  if (isLoading) {
    return (
      <div className="space-y-10">
        <Skeleton className="h-8 w-48" />
        {[0, 1, 2].map((i) => (
          <div key={i} className="grid gap-5 md:grid-cols-[1fr_2fr] md:gap-10">
            <Skeleton className="h-5 w-40" />
            <Skeleton className="h-24 w-full" />
          </div>
        ))}
      </div>
    )
  }

  if (error || !settings) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Could not load on-demand TLS settings</AlertTitle>
        <AlertDescription className="space-y-3">
          <p>
            {error instanceof Error
              ? error.message
              : 'The settings request failed.'}{' '}
            Reading them needs permission to read platform settings.
          </p>
          <Button variant="outline" size="sm" onClick={() => refetch()}>
            <RefreshCw className="mr-2 h-4 w-4" />
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  }

  const source: OnDemandTlsSettingsSource = settings
  const blockers = onDemandTlsBlockers(zone, source)
  const derivedZone = effectiveZone(zone, settings.external_url)
  const savedEnabled = settings.on_demand_tls?.enabled ?? false

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-10">
      <PageHeader
        title="On-demand TLS"
        description="Issue a Let's Encrypt certificate the first time a routed hostname is requested over HTTPS. Every attempt is listed under Certificates."
      />
      {/* Locked while saving: the form resets to the submitted values when
          the save completes, so edits made meanwhile would be lost. */}
      <fieldset
        disabled={isSubmitting}
        className="min-w-0 max-w-5xl space-y-10 disabled:opacity-70"
      >
        <SettingsGroup
          title="Issuance"
          description={`Saved state: ${savedEnabled ? 'on' : 'off'}. Changes apply after Temps restarts.`}
        >
          <div className="flex items-start justify-between gap-4">
            <div className="space-y-1">
              <Label htmlFor="on-demand-tls-enabled">
                Issue certificates on demand
              </Label>
              <p className="text-xs text-muted-foreground">
                Only stable hostnames get a certificate: environment URLs and
                the console. Per-deployment URLs never do.
              </p>
            </div>
            <Controller
              control={control}
              name="enabled"
              render={({ field }) => (
                <Switch
                  id="on-demand-tls-enabled"
                  checked={field.value}
                  onCheckedChange={field.onChange}
                />
              )}
            />
          </div>

          {enabled && blockers.length > 0 && (
            <Callout tone="warning" title="Issuance will not start yet">
              <ul className="space-y-2">
                {blockers.map((blocker) => (
                  <li key={blocker.id}>
                    {blocker.message}{' '}
                    {blocker.fixHref && blocker.fixLabel && (
                      <Link
                        to={blocker.fixHref}
                        className="font-medium text-foreground underline"
                      >
                        {blocker.fixLabel}
                      </Link>
                    )}
                  </li>
                ))}
              </ul>
            </Callout>
          )}

          <div className="space-y-2">
            <Label htmlFor="on-demand-tls-zone">Zone</Label>
            <Input
              id="on-demand-tls-zone"
              placeholder="apps.example.com"
              autoComplete="off"
              spellCheck={false}
              {...register('zone')}
            />
            <p className="text-xs text-muted-foreground">
              Direct subdomains of this domain are eligible, for example{' '}
              <code>myapp.apps.example.com</code> but not{' '}
              <code>a.myapp.apps.example.com</code>. Leave empty to use the
              external URL when it is a <code>*.sslip.io</code> address.{' '}
              {derivedZone
                ? `Effective zone: ${derivedZone}.`
                : 'No zone can be derived from the current external URL.'}
            </p>
            {errors.zone && (
              <p className="text-xs text-destructive">{errors.zone.message}</p>
            )}
          </div>
        </SettingsGroup>

        <SettingsGroup
          title="Limits"
          description="Caps that keep a burst of new hostnames from exhausting the Let's Encrypt rate limit."
        >
          <div className="grid gap-6 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="on-demand-tls-max-concurrent">
                Concurrent issuances
              </Label>
              <Input
                id="on-demand-tls-max-concurrent"
                type="number"
                min={MAX_CONCURRENT_RANGE.min}
                max={MAX_CONCURRENT_RANGE.max}
                {...register('max_concurrent', { valueAsNumber: true })}
              />
              <p className="text-xs text-muted-foreground">
                {MAX_CONCURRENT_RANGE.min}–{MAX_CONCURRENT_RANGE.max}. Default
                3.
              </p>
              {errors.max_concurrent && (
                <p className="text-xs text-destructive">
                  Must be a whole number from {MAX_CONCURRENT_RANGE.min} to{' '}
                  {MAX_CONCURRENT_RANGE.max}
                </p>
              )}
            </div>
            <div className="space-y-2">
              <Label htmlFor="on-demand-tls-hourly-cap">
                Issuances per hour
              </Label>
              <Input
                id="on-demand-tls-hourly-cap"
                type="number"
                min={HOURLY_CAP_RANGE.min}
                max={HOURLY_CAP_RANGE.max}
                {...register('hourly_cap', { valueAsNumber: true })}
              />
              <p className="text-xs text-muted-foreground">
                Across all hostnames. {HOURLY_CAP_RANGE.min}–
                {HOURLY_CAP_RANGE.max}. Default 10.
              </p>
              {errors.hourly_cap && (
                <p className="text-xs text-destructive">
                  Must be a whole number from {HOURLY_CAP_RANGE.min} to{' '}
                  {HOURLY_CAP_RANGE.max}
                </p>
              )}
            </div>
          </div>
        </SettingsGroup>

        <SettingsGroup
          title="Per-deployment URLs"
          description="Applies to the next request; no restart needed."
        >
          <Controller
            control={control}
            name="deployment_url_mode"
            render={({ field }) => (
              <RadioGroup
                value={field.value}
                onValueChange={field.onChange}
                className="grid gap-3 sm:grid-cols-2"
              >
                {MODE_OPTIONS.map((option) => (
                  <Label
                    key={option.value}
                    htmlFor={`deployment-url-mode-${option.value}`}
                    className="flex cursor-pointer items-start gap-3 rounded-md border p-4 font-normal has-[[data-state=checked]]:border-primary"
                  >
                    <RadioGroupItem
                      id={`deployment-url-mode-${option.value}`}
                      value={option.value}
                      className="mt-0.5"
                    />
                    <span className="space-y-1">
                      <span className="block text-sm font-medium">
                        {option.title}
                      </span>
                      <span className="block text-xs text-muted-foreground">
                        {option.description}
                      </span>
                    </span>
                  </Label>
                ))}
              </RadioGroup>
            )}
          />
        </SettingsGroup>
      </fieldset>

      <div className="sticky bottom-0 bg-background border-t pt-4 pb-2">
        <div className="flex flex-wrap justify-between items-center gap-3">
          <p className="text-sm text-muted-foreground">
            {isDirty ? 'You have unsaved changes' : 'All changes saved'}
          </p>
          <Button
            type="submit"
            busy={isSubmitting}
            busyLabel="Saving…"
            disabled={!isDirty && !isSubmitting}
          >
            <Save className="mr-2 h-4 w-4" />
            Save Changes
          </Button>
        </div>
      </div>
    </form>
  )
}
