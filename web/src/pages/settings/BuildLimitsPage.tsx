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
import { AlertCircle, Loader2, Save } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import type { BuildLimitsSettings } from '@/api/platformSettings'

interface BuildLimitsFormData {
  build_limits: BuildLimitsSettings
}

const DEFAULTS: BuildLimitsSettings = {
  max_concurrent: 2,
  cpu_limit_cores: 0,
  memory_limit_mb: 0,
}

export function BuildLimitsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    formState: { isDirty, isSubmitting, errors },
    reset,
  } = useForm<BuildLimitsFormData>({
    defaultValues: { build_limits: DEFAULTS },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Build Limits' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Build Limits')

  useEffect(() => {
    if (settings) {
      reset({
        build_limits: settings.build_limits || DEFAULTS,
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: BuildLimitsFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Build limits saved — applies to the next build')
    } catch {
      toast.error('Failed to save build limits')
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
          <CardTitle>Build limits</CardTitle>
          <CardDescription>
            Cap how many `docker build` operations run at the same time on the
            control plane and how much CPU/memory each is allowed to use.
            Prevents a burst of deploys from saturating the host. Worker nodes
            are not affected by these settings.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="grid gap-6 sm:grid-cols-3">
            <div className="space-y-2">
              <Label htmlFor="max_concurrent">Max concurrent builds</Label>
              <Input
                id="max_concurrent"
                type="number"
                min={1}
                max={32}
                {...register('build_limits.max_concurrent', {
                  valueAsNumber: true,
                  required: true,
                  min: 1,
                  max: 32,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Additional builds queue. Min 1, max 32. Default 2.
              </p>
              {errors.build_limits?.max_concurrent && (
                <p className="text-xs text-destructive">
                  Must be between 1 and 32
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="cpu_limit_cores">CPU per build (cores)</Label>
              <Input
                id="cpu_limit_cores"
                type="number"
                step={0.1}
                min={0}
                max={64}
                {...register('build_limits.cpu_limit_cores', {
                  valueAsNumber: true,
                  min: 0,
                  max: 64,
                })}
              />
              <p className="text-xs text-muted-foreground">
                E.g. 2.0 = 2 cores. 0 = use legacy 50%-of-host default.
              </p>
              {errors.build_limits?.cpu_limit_cores && (
                <p className="text-xs text-destructive">
                  Must be between 0 and 64
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="memory_limit_mb">Memory per build (MB)</Label>
              <Input
                id="memory_limit_mb"
                type="number"
                min={0}
                max={262144}
                {...register('build_limits.memory_limit_mb', {
                  valueAsNumber: true,
                  min: 0,
                  max: 262144,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Hard cap — builds that exceed this OOM-kill. 0 = use legacy
                50%-of-host default. Note: Docker BuildKit caps memory at ~2
                GB (i32 max bytes); higher values are silently truncated.
              </p>
              {errors.build_limits?.memory_limit_mb && (
                <p className="text-xs text-destructive">
                  Must be between 0 and 262144
                </p>
              )}
            </div>
          </div>

          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>How limits apply</AlertTitle>
            <AlertDescription>
              Concurrency takes effect on the next plugin restart (i.e. next
              <code className="mx-1 rounded bg-muted px-1">temps serve</code>
              start). Per-build CPU/memory caps apply to the very next build
              — no restart needed.
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
