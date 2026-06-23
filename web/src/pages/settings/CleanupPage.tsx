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
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertCircle, Loader2, Save } from 'lucide-react'
import { useEffect } from 'react'
import { Controller, useForm } from 'react-hook-form'
import { toast } from 'sonner'
import type { CleanupSettings } from '@/api/platformSettings'

interface CleanupFormData {
  cleanup: CleanupSettings
}

const DEFAULTS: CleanupSettings = {
  enabled: true,
  run_hour_utc: 2,
  image_max_age_days: 7,
  keep_deployments_per_env: 3,
  build_cache_max_age_days: 7,
  build_cache_max_size_mb: 0,
}

export function CleanupPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    control,
    formState: { isDirty, isSubmitting, errors },
    reset,
    watch,
  } = useForm<CleanupFormData>({
    defaultValues: { cleanup: DEFAULTS },
  })

  const enabled = watch('cleanup.enabled')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Cleanup' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Cleanup')

  useEffect(() => {
    if (settings) {
      reset({
        cleanup: settings.cleanup || DEFAULTS,
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: CleanupFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Cleanup settings saved — apply on the next scheduler tick')
    } catch {
      toast.error('Failed to save cleanup settings')
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
          <CardTitle>Docker cleanup scheduler</CardTitle>
          <CardDescription>
            Nightly job that reclaims disk space by removing old deployment
            images and stale BuildKit cache. Runs once per day at the
            configured UTC hour. Changes take effect on the next scheduler
            tick — no server restart needed.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="flex items-center gap-3">
            <Controller
              name="cleanup.enabled"
              control={control}
              render={({ field }) => (
                <Switch
                  id="cleanup_enabled"
                  checked={field.value}
                  onCheckedChange={field.onChange}
                />
              )}
            />
            <Label htmlFor="cleanup_enabled">
              Enable nightly cleanup
            </Label>
          </div>

          <div className="grid gap-6 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="run_hour_utc">Run hour (UTC, 0–23)</Label>
              <Input
                id="run_hour_utc"
                type="number"
                min={0}
                max={23}
                disabled={!enabled}
                {...register('cleanup.run_hour_utc', {
                  valueAsNumber: true,
                  required: true,
                  min: 0,
                  max: 23,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Hour the scheduler fires each day. Default 2 (02:00 UTC).
              </p>
              {errors.cleanup?.run_hour_utc && (
                <p className="text-xs text-destructive">Must be 0–23</p>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Deployment image retention</CardTitle>
          <CardDescription>
            Tagged deployment images (
            <code className="rounded bg-muted px-1">slug:latest</code>) are
            never reclaimed by the standard Docker dangling-image prune.
            These settings control when Temps removes them.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="grid gap-6 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="image_max_age_days">
                Minimum age before removal (days)
              </Label>
              <Input
                id="image_max_age_days"
                type="number"
                min={1}
                max={365}
                disabled={!enabled}
                {...register('cleanup.image_max_age_days', {
                  valueAsNumber: true,
                  required: true,
                  min: 1,
                  max: 365,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Images younger than this are always kept. Default 7.
              </p>
              {errors.cleanup?.image_max_age_days && (
                <p className="text-xs text-destructive">Must be 1–365</p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="keep_deployments_per_env">
                Keep N most-recent per environment
              </Label>
              <Input
                id="keep_deployments_per_env"
                type="number"
                min={1}
                max={50}
                disabled={!enabled}
                {...register('cleanup.keep_deployments_per_env', {
                  valueAsNumber: true,
                  required: true,
                  min: 1,
                  max: 50,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Safeguards rollbacks. Default 3 (keeps last 3 per env).
              </p>
              {errors.cleanup?.keep_deployments_per_env && (
                <p className="text-xs text-destructive">Must be 1–50</p>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>BuildKit cache</CardTitle>
          <CardDescription>
            BuildKit accumulates layer cache across all builds. Large caches
            speed up rebuilds but can fill disk on busy nodes.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="grid gap-6 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="build_cache_max_age_days">
                Prune cache older than (days)
              </Label>
              <Input
                id="build_cache_max_age_days"
                type="number"
                min={1}
                max={365}
                disabled={!enabled}
                {...register('cleanup.build_cache_max_age_days', {
                  valueAsNumber: true,
                  required: true,
                  min: 1,
                  max: 365,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Cache entries not accessed within this window are pruned.
                Default 7.
              </p>
              {errors.cleanup?.build_cache_max_age_days && (
                <p className="text-xs text-destructive">Must be 1–365</p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="build_cache_max_size_mb">
                Hard size cap (MiB, 0 = no cap)
              </Label>
              <Input
                id="build_cache_max_size_mb"
                type="number"
                min={0}
                disabled={!enabled}
                {...register('cleanup.build_cache_max_size_mb', {
                  valueAsNumber: true,
                  min: 0,
                })}
              />
              <p className="text-xs text-muted-foreground">
                If total build-cache size exceeds this, all cache is dropped
                (hard prune). 0 disables the cap.
              </p>
              {errors.cleanup?.build_cache_max_size_mb && (
                <p className="text-xs text-destructive">Must be ≥ 0</p>
              )}
            </div>
          </div>

          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>containerd vs Docker</AlertTitle>
            <AlertDescription>
              These settings only affect the Docker daemon. If your node uses
              containerd directly (no Docker socket), Temps logs a warning
              and skips Docker cleanup — reclaim
              <code className="mx-1 rounded bg-muted px-1">
                /var/lib/containerd
              </code>
              manually or via a cron job.
            </AlertDescription>
          </Alert>
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
