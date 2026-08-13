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
import type { RequestTimeoutSettings } from '@/api/client/types.gen'
import { AlertCircle, Loader2, Save, Timer } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

interface RequestTimeoutsFormData {
  request_timeouts: RequestTimeoutSettings
}

const DEFAULTS: RequestTimeoutSettings = {
  max_request_timeout_seconds: 600,
  default_http_timeout_seconds: 0,
  default_sse_idle_timeout_seconds: 0,
  default_websocket_idle_timeout_seconds: 0,
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
    handleSubmit,
    formState: { isDirty, isSubmitting, errors },
    reset,
  } = useForm<RequestTimeoutsFormData>({
    defaultValues: { request_timeouts: DEFAULTS },
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
            How long the proxy waits on upstream app traffic before closing
            the connection. Timeouts are opt-in — 0 means no timeout, and
            that&apos;s the default for every traffic class, so existing apps
            are unaffected until you configure one. Server-Sent Events and
            WebSocket connections get their own idle timeout since
            they&apos;re long-lived by design — a plain HTTP request uses the
            regular timeout instead. Projects and environments can set their
            own override under Deployment Config; the ceiling below only
            applies once a timeout is actually configured, and never longer
            than that.
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
              this — but only applies once a timeout is actually configured.
              Min 5, max 86400 (24h). Default 600 (10m).
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
