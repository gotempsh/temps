// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { Switch } from '@/components/ui/switch'
import type {
  RequestTimeoutSettings,
  ConnectionLimitSettings,
  TenantResourceCeilings,
} from '@/api/client/types.gen'
import { AlertCircle, Gauge, Loader2, Save, ShieldCheck, Timer } from 'lucide-react'
import { Controller } from 'react-hook-form'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

interface RequestTimeoutsFormData {
  request_timeouts: RequestTimeoutSettings
  connection_limits: ConnectionLimitSettings
  tenant_resource_ceilings: TenantResourceCeilings
}

const DEFAULTS: RequestTimeoutSettings = {
  max_request_timeout_seconds: 600,
  default_http_timeout_seconds: 0,
  default_sse_idle_timeout_seconds: 0,
  default_websocket_idle_timeout_seconds: 0,
}

const CONNECTION_LIMIT_DEFAULTS: ConnectionLimitSettings = {
  default_max_concurrent_connections: 0,
}

/**
 * Unenforced, deliberately. The defaults above are *instance defaults* a
 * project can override — including overriding them to "unlimited". These
 * ceilings are the bound on those overrides, and leaving them off means an
 * upgrade changes nothing for anyone.
 */
const CEILING_DEFAULTS: TenantResourceCeilings = {
  max_memory_limit_mb: 0,
  max_concurrent_connections: 0,
  allow_unlimited_request_timeouts: true,
}

/**
 * Upstream request/connection timeouts the proxy applies to customer app
 * traffic. Timeouts are opt-in: by default no timeout is applied to any
 * traffic class (0 = no timeout), so an existing app with a slow endpoint or
 * long-lived connection keeps working unchanged. A project or environment
 * may set its own override under Deployment Config; the hard ceiling below
 * only takes effect once a timeout is actually configured (here or per
 * project/environment) — it never creates one on its own.
 */
export function RequestTimeoutsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    control,
    handleSubmit,
    formState: { isDirty, isSubmitting, errors },
    reset,
  } = useForm<RequestTimeoutsFormData>({
    defaultValues: {
      request_timeouts: DEFAULTS,
      connection_limits: CONNECTION_LIMIT_DEFAULTS,
      tenant_resource_ceilings: CEILING_DEFAULTS,
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Request Timeouts' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Request Timeouts')

  useEffect(() => {
    if (settings) {
      reset({
        request_timeouts: settings.request_timeouts || DEFAULTS,
        connection_limits:
          settings.connection_limits || CONNECTION_LIMIT_DEFAULTS,
        tenant_resource_ceilings:
          settings.tenant_resource_ceilings || CEILING_DEFAULTS,
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: RequestTimeoutsFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Request timeouts saved — applies to the next request')
    } catch {
      toast.error('Failed to save request timeouts')
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
        <AlertDescription>Failed to load settings.</AlertDescription>
      </Alert>
    )
  }

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Timer className="h-5 w-5" />
            Request Timeouts
          </CardTitle>
          <CardDescription>
            How long the proxy waits on upstream app traffic before closing the
            connection. Timeouts are opt-in — 0 means no timeout, and
            that&apos;s the default for every traffic class, so existing apps
            are unaffected until you configure one. Server-Sent Events and
            WebSocket connections get their own idle timeout since they&apos;re
            long-lived by design — a plain HTTP request uses the regular timeout
            instead. Projects and environments can set their own override under
            Deployment Config; the ceiling below only applies once a timeout is
            actually configured, and never longer than that.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="space-y-2 max-w-xs">
            <Label htmlFor="max_request_timeout_seconds">
              Hard ceiling (seconds)
            </Label>
            <Input
              id="max_request_timeout_seconds"
              type="number"
              min={5}
              max={86400}
              {...register('request_timeouts.max_request_timeout_seconds', {
                valueAsNumber: true,
                required: true,
                min: 5,
                max: 86400,
              })}
            />
            <p className="text-xs text-muted-foreground">
              No project/environment override, and no default below, can exceed
              this — but only applies once a timeout is actually configured. Min
              5, max 86400 (24h). Default 600 (10m).
            </p>
            {errors.request_timeouts?.max_request_timeout_seconds && (
              <p className="text-xs text-destructive">
                Must be between 5 and 86400 seconds
              </p>
            )}
          </div>

          <div className="grid gap-6 sm:grid-cols-3">
            <div className="space-y-2">
              <Label htmlFor="default_http_timeout_seconds">
                Regular HTTP (seconds)
              </Label>
              <Input
                id="default_http_timeout_seconds"
                type="number"
                min={0}
                {...register('request_timeouts.default_http_timeout_seconds', {
                  valueAsNumber: true,
                  required: true,
                  min: 0,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Non-streaming requests. 0 = no timeout (default).
              </p>
              {errors.request_timeouts?.default_http_timeout_seconds && (
                <p className="text-xs text-destructive">
                  Must be 0 (no timeout) or greater
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="default_sse_idle_timeout_seconds">
                SSE idle (seconds)
              </Label>
              <Input
                id="default_sse_idle_timeout_seconds"
                type="number"
                min={0}
                {...register(
                  'request_timeouts.default_sse_idle_timeout_seconds',
                  { valueAsNumber: true, required: true, min: 0 }
                )}
              />
              <p className="text-xs text-muted-foreground">
                Server-Sent Events streams. 0 = no timeout (default).
              </p>
              {errors.request_timeouts?.default_sse_idle_timeout_seconds && (
                <p className="text-xs text-destructive">
                  Must be 0 (no timeout) or greater
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="default_websocket_idle_timeout_seconds">
                WebSocket idle (seconds)
              </Label>
              <Input
                id="default_websocket_idle_timeout_seconds"
                type="number"
                min={0}
                {...register(
                  'request_timeouts.default_websocket_idle_timeout_seconds',
                  { valueAsNumber: true, required: true, min: 0 }
                )}
              />
              <p className="text-xs text-muted-foreground">
                WebSocket connections. 0 = no timeout (default).
              </p>
              {errors.request_timeouts
                ?.default_websocket_idle_timeout_seconds && (
                <p className="text-xs text-destructive">
                  Must be 0 (no timeout) or greater
                </p>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Gauge className="h-5 w-5" />
            Concurrent Connection Limit
          </CardTitle>
          <CardDescription>
            Caps how many concurrent in-flight requests the proxy allows to a
            single project/environment&apos;s upstream, independent of the
            timeouts above — protects the proxy&apos;s own connection budget
            from a single stalled or malicious app. 0 = unlimited, and
            that&apos;s the default, so existing apps are unaffected until you
            configure a limit. Matters most when multiple apps share one
            node/instance — a project or environment can override this under
            Deployment Config.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-2 max-w-xs">
            <Label htmlFor="default_max_concurrent_connections">
              Max concurrent connections
            </Label>
            <Input
              id="default_max_concurrent_connections"
              type="number"
              min={0}
              {...register(
                'connection_limits.default_max_concurrent_connections',
                { valueAsNumber: true, required: true, min: 0 }
              )}
            />
            <p className="text-xs text-muted-foreground">
              0 = unlimited (default). Requests over the limit get an immediate
              503 instead of queuing.
            </p>
            {errors.connection_limits?.default_max_concurrent_connections && (
              <p className="text-xs text-destructive">
                Must be 0 (unlimited) or greater
              </p>
            )}
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <ShieldCheck className="h-5 w-5" />
            Project Override Ceilings
          </CardTitle>
          <CardDescription>
            The two settings above are <em>defaults</em> — anyone who can edit a
            project or environment&apos;s Deployment Config can override them,
            including overriding them to unlimited. These ceilings bound those
            overrides. All three are off by default, so nothing changes until
            you set one, and holders of the Settings write permission are never
            blocked by them. An override that breaks a ceiling is rejected with
            an explanation, never silently reduced.
            <br />
            <br />
            <strong>Applied when a config is saved, not retroactively.</strong>{' '}
            Setting a ceiling here does not change projects that already exceed
            it — they keep running as configured until someone next edits them.
            Note also that the memory ceiling is per container, so a project
            with several replicas can still total more than the ceiling.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="grid gap-6 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="max_memory_limit_mb">
                Max memory limit (MB)
              </Label>
              <Input
                id="max_memory_limit_mb"
                type="number"
                min={0}
                {...register('tenant_resource_ceilings.max_memory_limit_mb', {
                  valueAsNumber: true,
                  required: true,
                  min: 0,
                })}
              />
              <p className="text-xs text-muted-foreground">
                0 = no ceiling (default). When set, a project cannot request
                more than this, nor set its memory limit to unlimited.
              </p>
              {errors.tenant_resource_ceilings?.max_memory_limit_mb && (
                <p className="text-xs text-destructive">
                  Must be 0 (no ceiling) or greater
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="max_concurrent_connections">
                Max concurrent connections
              </Label>
              <Input
                id="max_concurrent_connections"
                type="number"
                min={0}
                {...register(
                  'tenant_resource_ceilings.max_concurrent_connections',
                  { valueAsNumber: true, required: true, min: 0 }
                )}
              />
              <p className="text-xs text-muted-foreground">
                0 = no ceiling (default). Bounds the per-project override of the
                connection limit above.
              </p>
              {errors.tenant_resource_ceilings?.max_concurrent_connections && (
                <p className="text-xs text-destructive">
                  Must be 0 (no ceiling) or greater
                </p>
              )}
            </div>
          </div>

          <div className="flex items-start justify-between gap-4">
            <div className="space-y-1">
              <Label htmlFor="allow_unlimited_request_timeouts">
                Allow projects to disable timeouts
              </Label>
              <p className="text-xs text-muted-foreground">
                On by default. Turn it off to stop a project from setting its
                request, SSE, or WebSocket timeout to 0 — the value that opts
                out of the hard ceiling above entirely. Timeouts a project sets
                to a real number are already clamped to that ceiling.
              </p>
            </div>
            <Controller
              control={control}
              name="tenant_resource_ceilings.allow_unlimited_request_timeouts"
              render={({ field }) => (
                <Switch
                  id="allow_unlimited_request_timeouts"
                  checked={field.value}
                  onCheckedChange={field.onChange}
                />
              )}
            />
          </div>
        </CardContent>
      </Card>

      {isDirty && (
        <div className="sticky bottom-0 bg-background border-t pt-4 pb-2">
          <div className="flex justify-between items-center">
            <p className="text-sm text-muted-foreground">
              You have unsaved changes
            </p>
            <Button type="submit" disabled={isSubmitting}>
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
