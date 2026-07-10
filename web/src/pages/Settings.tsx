import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
  Globe,
  Image,
  Link,
  Loader2,
  RefreshCw,
  Save,
  ShieldCheck,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'

type SettingsFormData = Pick<
  PlatformSettings,
  | 'external_url'
  | 'internal_url'
  | 'preview_domain'
  | 'screenshots'
  | 'letsencrypt'
>

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
      await updateSettings.mutateAsync(data)
      reset(data)
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
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Link className="h-5 w-5" />
            External URL
          </CardTitle>
          <CardDescription>
            Set the external URL for your platform
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            <Label htmlFor="external-url">External URL</Label>
            <Input
              id="external-url"
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
            {errors.external_url && (
              <p className="text-sm text-destructive">
                {errors.external_url.message}
              </p>
            )}
            <p className="text-sm text-muted-foreground">
              Used for OAuth callbacks, webhooks, and external integrations
            </p>
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
              How service containers reach the Temps API from inside the Docker
              network (OTLP metrics ingest, agent callbacks). Leave blank to use{' '}
              <code className="font-mono text-xs">
                http://host.docker.internal:&lt;proxy-port&gt;
              </code>
              .
            </p>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Globe className="h-5 w-5" />
            Preview Domain
          </CardTitle>
          <CardDescription>
            Configure the domain used for deployment previews
          </CardDescription>
        </CardHeader>
        <CardContent>
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
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <ShieldCheck className="h-5 w-5" />
            Let&apos;s Encrypt
          </CardTitle>
          <CardDescription>
            Contact email for automatic TLS certificate issuance and renewal
          </CardDescription>
        </CardHeader>
        <CardContent>
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
            <p className="text-sm text-muted-foreground">
              Let&apos;s Encrypt requires a real contact email to register an
              ACME account. Used for all certificate provisioning and background
              auto-renewal (HTTP-01 and DNS-01).
            </p>
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
            <p className="text-sm text-muted-foreground">
              Staging certificates are not trusted by browsers — use only for
              testing to avoid Let&apos;s Encrypt&apos;s production rate limits.
            </p>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Image className="h-5 w-5" />
            Screenshots
          </CardTitle>
          <CardDescription>
            Configure screenshot generation for deployments
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
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
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <RefreshCw className="h-5 w-5" />
            Route Table
          </CardTitle>
          <CardDescription>
            Manually refresh the proxy route table from the database. Use this
            if routes appear out of sync after deployments or configuration
            changes.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Button
            type="button"
            variant="outline"
            disabled={isRefreshingRoutes}
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
        </CardContent>
      </Card>

      {isDirty && (
        <div className="sticky bottom-0 bg-background border-t pt-4 pb-2">
          <div className="flex flex-col gap-2 sm:flex-row sm:justify-between sm:items-center">
            <p className="text-sm text-muted-foreground">
              You have unsaved changes
            </p>
            <Button
              type="submit"
              disabled={isSubmitting}
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
      )}
    </form>
  )
}
