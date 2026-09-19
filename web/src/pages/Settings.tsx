// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button, Disclosure, Field, FormErrors } from '@temps-sdk/ds'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { SettingsSection } from '@/components/ui/settings-section'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  useSettings,
  useUpdateSettings,
  type PlatformSettings,
} from '@/hooks/useSettings'
import { client } from '@/api/client/client.gen'
import {
  AlertCircle,
  Loader2,
  RefreshCw,
  Save,
  ShieldCheck,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { PageHeader } from '@/components/layout/PageContainer'

type SettingsFormData = Pick<
  PlatformSettings,
  | 'external_url'
  | 'internal_url'
  | 'preview_domain'
  | 'edge_target'
  | 'screenshots'
  | 'letsencrypt'
  | 'console_force_https'
>

function optionalString(value: string | null | undefined): string | null {
  const trimmed = value?.trim() ?? ''
  return trimmed.length > 0 ? trimmed : null
}

export function Settings() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()
  const [isRefreshingRoutes, setIsRefreshingRoutes] = useState(false)

  const {
    register,
    handleSubmit,
    control,
    formState: { isDirty, isSubmitting, errors },
    reset,
    setValue,
  } = useForm<SettingsFormData>({
    defaultValues: {
      external_url: '',
      internal_url: '',
      preview_domain: 'localho.st',
      edge_target: '',
      console_force_https: null,
      screenshots: {
        enabled: false,
        provider: 'local',
        url: '',
      },
      letsencrypt: {
        email: '',
        environment: 'production',
      },
    },
  })

  const screenshots = useWatch({ control, name: 'screenshots' })
  const consoleForceHttps = useWatch({ control, name: 'console_force_https' })
  // Tri-state, so a Switch can't represent it: null ("inherit the certificate
  // heuristic") is a genuinely different answer from false ("never redirect").
  const consoleForceHttpsValue =
    consoleForceHttps === null || consoleForceHttps === undefined
      ? 'auto'
      : consoleForceHttps
        ? 'always'
        : 'never'
  const letsencryptEnvironment = useWatch({
    control,
    name: 'letsencrypt.environment',
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings' }])
  }, [setBreadcrumbs])

  usePageTitle('Settings')

  useEffect(() => {
    if (settings) {
      reset({
        external_url: settings.external_url || '',
        internal_url: settings.internal_url || '',
        preview_domain: settings.preview_domain || 'localho.st',
        edge_target: settings.edge_target || '',
        console_force_https: settings.console_force_https ?? null,
        screenshots: settings.screenshots || {
          enabled: false,
          provider: 'local',
          url: '',
        },
        letsencrypt: {
          email: settings.letsencrypt?.email || '',
          environment: settings.letsencrypt?.environment || 'production',
        },
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: SettingsFormData) => {
    try {
      const normalized: SettingsFormData = {
        ...data,
        edge_target: optionalString(data.edge_target),
      }
      await updateSettings.mutateAsync(normalized)
      reset({
        ...normalized,
        edge_target: normalized.edge_target || '',
      })
      toast.success('Settings saved successfully')
    } catch (err: any) {
      const detail =
        err?.body?.detail ||
        err?.message ||
        'Failed to save settings. Please try again.'
      toast.error(detail)
    }
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>
          Failed to load settings. Please try again later.
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
      <PageHeader title="Settings" />
      <div className="max-w-2xl space-y-10">
        <section
          aria-labelledby="platform-settings-heading"
          className="space-y-5"
        >
          <h2
            id="platform-settings-heading"
            className="text-base font-semibold"
          >
            Platform
          </h2>
          <Field
            label="External URL"
            optional
            error={errors.external_url?.message}
            help={{
              label: 'About the external URL',
              content:
                'Used for OAuth callbacks, webhooks, and external integrations.',
            }}
          >
            {(fieldProps) => (
              <Input
                {...fieldProps}
                type="url"
                placeholder="https://your-domain.com"
                {...register('external_url', {
                  validate: (value) => {
                    if (!value) return true // optional
                    const trimmed = value.trim()
                    if (!trimmed) return true
                    if (
                      !trimmed.startsWith('http://') &&
                      !trimmed.startsWith('https://')
                    )
                      return 'Must start with http:// or https://'
                    if (trimmed.includes('#') || trimmed.includes('?'))
                      return 'Must not contain # or ? characters'
                    try {
                      new URL(trimmed)
                    } catch {
                      return 'Must be a valid URL'
                    }
                    return true
                  },
                })}
              />
            )}
          </Field>

          <div className="space-y-2">
            <Label htmlFor="preview-domain">Preview Domain</Label>
            <Input
              id="preview-domain"
              type="text"
              placeholder="localho.st"
              {...register('preview_domain')}
            />
            <p className="text-sm text-muted-foreground">
              Deployments will be accessible at subdomain.
              {settings?.preview_domain || 'localho.st'}
            </p>
          </div>
          <SettingsSection
            title="Advanced networking"
            icon={ShieldCheck}
            hasError={Boolean(errors.internal_url)}
          >
            <div className="space-y-2 pt-4">
              <Label htmlFor="console-force-https">
                Redirect console to HTTPS
              </Label>
              <Select
                value={consoleForceHttpsValue}
                onValueChange={(value) =>
                  setValue(
                    'console_force_https',
                    value === 'auto' ? null : value === 'always',
                    { shouldDirty: true }
                  )
                }
              >
                <SelectTrigger
                  id="console-force-https"
                  className="w-full sm:w-[280px]"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">
                    Automatic — once a certificate exists
                  </SelectItem>
                  <SelectItem value="always">Always redirect</SelectItem>
                  <SelectItem value="never">Never redirect</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-sm text-muted-foreground">
                Use Always only when Temps terminates TLS. A CDN or reverse
                proxy terminating TLS can cause redirect loops.
              </p>
              <Disclosure label="How automatic redirects work">
                <p>
                  Automatic redirects HTTP requests only after Temps has issued
                  a certificate for the console hostname. HTTP-only
                  installations keep working.
                </p>
              </Disclosure>
            </div>

            <div className="space-y-2 pt-4">
              <Label htmlFor="internal-url">Internal URL</Label>
              <Input
                id="internal-url"
                type="url"
                placeholder="http://host.docker.internal:8080"
                {...register('internal_url', {
                  validate: (value) => {
                    if (!value) return true // optional — falls back to default
                    const trimmed = value.trim()
                    if (!trimmed) return true
                    if (
                      !trimmed.startsWith('http://') &&
                      !trimmed.startsWith('https://')
                    )
                      return 'Must start with http:// or https://'
                    if (trimmed.includes('#') || trimmed.includes('?'))
                      return 'Must not contain # or ? characters'
                    try {
                      new URL(trimmed)
                    } catch {
                      return 'Must be a valid URL'
                    }
                    return true
                  },
                })}
              />
              {errors.internal_url && (
                <p className="text-sm text-destructive">
                  {errors.internal_url.message}
                </p>
              )}
              <p className="text-sm text-muted-foreground">
                How service containers reach the Temps API from inside the
                Docker network (OTLP metrics ingest, agent callbacks). Leave
                blank to use{' '}
                <code className="font-mono text-xs">
                  http://host.docker.internal:&lt;proxy-port&gt;
                </code>
                .
              </p>
            </div>
          </SettingsSection>
        </section>

        <section
          aria-labelledby="certificate-settings-heading"
          className="space-y-5"
        >
          <h2
            id="certificate-settings-heading"
            className="text-base font-semibold"
          >
            Certificates
          </h2>
          <div className="space-y-2">
            <Label htmlFor="letsencrypt-email">Contact Email</Label>
            <Input
              id="letsencrypt-email"
              type="email"
              placeholder="ops@your-domain.com"
              {...register('letsencrypt.email', {
                validate: (value) => {
                  if (!value) return true // optional, but renewals will fail without it
                  const trimmed = value.trim()
                  if (!trimmed) return true
                  if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(trimmed))
                    return 'Must be a valid email address'
                  return true
                },
              })}
            />
            {errors.letsencrypt?.email && (
              <p className="text-sm text-destructive">
                {errors.letsencrypt.email.message}
              </p>
            )}
            {!settings?.letsencrypt?.email && (
              <p className="text-sm text-amber-600 dark:text-amber-500">
                No contact email configured — certificate issuance and automatic
                renewal will fail until this is set.
              </p>
            )}
            <Disclosure label="How certificate provisioning works">
              <p>
                Let&apos;s Encrypt uses this email to register the account used
                for certificate issuance and automatic renewal.
              </p>
            </Disclosure>
          </div>

          <div className="space-y-2 pt-4">
            <Label htmlFor="letsencrypt-environment">Environment</Label>
            <Select
              value={letsencryptEnvironment}
              onValueChange={(value: 'production' | 'staging') =>
                setValue('letsencrypt.environment', value, {
                  shouldDirty: true,
                })
              }
            >
              <SelectTrigger id="letsencrypt-environment">
                <SelectValue placeholder="Select environment" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="production">Production</SelectItem>
                <SelectItem value="staging">
                  Staging (testing, avoids rate limits)
                </SelectItem>
              </SelectContent>
            </Select>
            {letsencryptEnvironment === 'staging' && (
              <p className="text-sm text-muted-foreground">
                Staging certificates are not trusted by browsers. Use only for
                testing.
              </p>
            )}
          </div>

          <SettingsSection title="Advanced DNS settings" icon={ShieldCheck}>
            <div className="space-y-2">
              <Label htmlFor="edge-target">Edge target (for DNS sync)</Label>
              <Input
                id="edge-target"
                type="text"
                placeholder="203.0.113.10 or edge.example.com"
                {...register('edge_target')}
              />
              <p className="text-sm text-muted-foreground">
                Public address that generated DNS records point at when a
                managed domain opts into record sync. An IP creates A/AAAA
                records; a hostname creates CNAME records. Leave blank to
                disable DNS sync. The Standard vs Flat hostname layout is
                configured per managed domain under DNS providers.
              </p>
            </div>
          </SettingsSection>
        </section>

        <section
          aria-labelledby="screenshot-settings-heading"
          className="space-y-5"
        >
          <h2
            id="screenshot-settings-heading"
            className="text-base font-semibold"
          >
            Screenshots
          </h2>
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <div className="space-y-0.5">
                <Label htmlFor="screenshots-enabled">Enable Screenshots</Label>
                <p className="text-sm text-muted-foreground">
                  Generate screenshots of deployments for previews
                </p>
              </div>
              <Switch
                id="screenshots-enabled"
                checked={screenshots?.enabled}
                onCheckedChange={(checked) =>
                  setValue('screenshots.enabled', checked, {
                    shouldDirty: true,
                  })
                }
              />
            </div>

            {screenshots?.enabled && (
              <>
                <div className="space-y-2">
                  <Label htmlFor="screenshot-provider">Provider</Label>
                  <Select
                    value={screenshots?.provider}
                    onValueChange={(value: 'local' | 'external') =>
                      setValue('screenshots.provider', value, {
                        shouldDirty: true,
                      })
                    }
                  >
                    <SelectTrigger id="screenshot-provider">
                      <SelectValue placeholder="Select provider" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="local">
                        Local Screenshot Service
                      </SelectItem>
                      <SelectItem value="external">
                        External Screenshot API
                      </SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                {screenshots.provider === 'external' && (
                  <div className="space-y-2">
                    <Label htmlFor="screenshot-url">Screenshot API URL</Label>
                    <Input
                      id="screenshot-url"
                      type="url"
                      placeholder="https://<your-domain>/api/screenshot?url={url}&width=1920&height=1080"
                      {...register('screenshots.url')}
                    />
                    <p className="text-sm text-muted-foreground">
                      Configure your API endpoint with{' '}
                      <code className="px-1 py-0.5 bg-muted rounded text-xs">
                        {'{url}'}
                      </code>{' '}
                      placeholder.
                    </p>
                  </div>
                )}
              </>
            )}
          </div>
        </section>

        <Disclosure label="Troubleshooting">
          <p className="mb-3 text-sm text-muted-foreground">
            Refresh proxy routes if a deployment or configuration change is out
            of sync.
          </p>
          <Button
            type="button"
            variant="outline"
            busy={isRefreshingRoutes}
            busyLabel="Refreshing…"
            onClick={async () => {
              setIsRefreshingRoutes(true)
              try {
                const response = await client.post({
                  url: '/settings/routes/refresh',
                  security: [{ scheme: 'bearer', type: 'http' }],
                })
                const data = response.data as
                  { route_count: number; message: string } | undefined
                toast.success(
                  data?.message || 'Route table refreshed successfully'
                )
              } catch {
                toast.error('Failed to refresh route table')
              } finally {
                setIsRefreshingRoutes(false)
              }
            }}
          >
            {isRefreshingRoutes ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                Refreshing...
              </>
            ) : (
              <>
                <RefreshCw className="mr-2 h-4 w-4" />
                Refresh Routes
              </>
            )}
          </Button>
        </Disclosure>
      </div>

      <FormErrors
        errors={{
          'External URL': errors.external_url?.message,
          'Internal URL': errors.internal_url?.message,
          'Contact email': errors.letsencrypt?.email?.message,
        }}
      />
      <div className="sticky bottom-0 bg-background border-t pt-4 pb-2">
        <div className="flex flex-col gap-2 sm:flex-row sm:justify-between sm:items-center">
          <p className="text-sm text-muted-foreground">
            {isDirty ? 'You have unsaved changes' : 'All changes saved'}
          </p>
          <Button
            type="submit"
            busy={isSubmitting}
            busyLabel="Saving…"
            disabled={!isDirty && !isSubmitting}
            className="w-full sm:w-auto"
          >
            {isSubmitting ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                Saving...
              </>
            ) : (
              <>
                <Save className="mr-2 h-4 w-4" />
                Save Changes
              </>
            )}
          </Button>
        </div>
      </div>
    </form>
  )
}
