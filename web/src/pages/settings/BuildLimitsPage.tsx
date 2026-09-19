// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@temps-sdk/ds'
import { PageHeader, SettingsGroup } from '@temps-sdk/ds'
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
      toast.success('Build limits saved, applied on the next temps serve start')
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
      <PageHeader title="Build limits" />
      <div className="max-w-5xl space-y-10">
        <SettingsGroup title="Build capacity">
          <p className="text-sm text-muted-foreground">
            Control-plane builds only. Worker nodes are not affected.
          </p>
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
              Legacy builder only; ignored by BuildKit. 0 uses 50% of host CPU.
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
              Ignored by BuildKit. 0 uses 50% of host memory; values above 2047
              MB are capped at 2047 MB.
            </p>
            {errors.build_limits?.memory_limit_mb && (
              <p className="text-xs text-destructive">
                Must be between 0 and 262144
              </p>
            )}
          </div>
        </SettingsGroup>
        <SettingsGroup title="Applying changes">
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Restart required</AlertTitle>
            <AlertDescription>
              All three settings take effect on the next
              <code className="mx-1 rounded bg-muted px-1">temps serve</code>
              start.
            </AlertDescription>
          </Alert>
        </SettingsGroup>
      </div>

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
