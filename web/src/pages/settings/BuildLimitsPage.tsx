// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SettingsFormSkeleton } from '@/components/settings/SettingsFormSkeleton'
import { SettingsLoadFailure } from '@/components/settings/SettingsLoadFailure'
import { isForbiddenRead } from '@/lib/read-failure'
import { SaveRow } from '@temps-sdk/ds'
import { PageHeader, SettingsGroup } from '@temps-sdk/ds'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettingsPage, useUpdateSettings } from '@/hooks/useSettings'
import { CiBuilderPanel } from '@/components/nodes/CiBuilderPanel'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import type { BuildLimitsSettings } from '@/api/platformSettings'
import {
  buildLimitsFormSchema,
  type BuildLimitsFormData,
} from '@/lib/build-limits'

const DEFAULTS: BuildLimitsSettings = {
  max_concurrent: 2,
  cpu_limit_cores: 0,
  memory_limit_mb: 0,
  build_cache_max_gb: 10,
  build_cache_max_unused_days: 7,
}

export function BuildLimitsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error, refetch } = useSettingsPage()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    formState: { isDirty, isSubmitting, errors },
    reset,
  } = useForm<BuildLimitsFormData>({
    resolver: zodResolver(buildLimitsFormSchema),
    defaultValues: { build_limits: DEFAULTS },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Build limits' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Build limits')

  useEffect(() => {
    if (settings) {
      reset({
        build_limits: { ...DEFAULTS, ...settings.build_limits },
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: BuildLimitsFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success(
        'Build limits saved and applied. Cache limits take effect at the next nightly cleanup'
      )
    } catch {
      // useUpdateSettings already toasts this failure, with the server's reason.
    }
  }

  if (isLoading) {
    return (
      <SettingsFormSkeleton title="Build limits" label="Loading build limits" />
    )
  }

  if (error) {
    return (
      <SettingsLoadFailure
        error={error}
        onRetry={isForbiddenRead(error) ? undefined : () => void refetch()}
      />
    )
  }

  return (
    <div className="space-y-10">
      <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
        <PageHeader title="Build limits" />
        <div className="max-w-5xl space-y-10">
          <SettingsGroup title="Build capacity">
            <p className="text-sm text-muted-foreground">
              Control-plane builds only. Worker nodes are not affected. Changes
              apply immediately: running builds finish with the limits they
              started with, queued and new builds use the new ones.
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
                })}
              />
              <p className="text-xs text-muted-foreground">
                Additional builds queue. Min 1, max 32. Default 2.
              </p>
              {errors.build_limits?.max_concurrent && (
                <p className="text-xs text-destructive">
                  {errors.build_limits.max_concurrent.message}
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
                })}
              />
              <p className="text-xs text-muted-foreground">
                Legacy builder only; ignored by BuildKit. 0 uses 50% of host
                CPU.
              </p>
              {errors.build_limits?.cpu_limit_cores && (
                <p className="text-xs text-destructive">
                  {errors.build_limits.cpu_limit_cores.message}
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
                })}
              />
              <p className="text-xs text-muted-foreground">
                Ignored by BuildKit. 0 uses 50% of host memory; values above
                2047 MB are capped at 2047 MB.
              </p>
              {errors.build_limits?.memory_limit_mb && (
                <p className="text-xs text-destructive">
                  {errors.build_limits.memory_limit_mb.message}
                </p>
              )}
            </div>
          </SettingsGroup>
          <SettingsGroup title="Build cache">
            <p className="text-sm text-muted-foreground">
              Docker keeps layers from earlier builds to speed up later ones.
              The nightly cleanup (2:00 UTC) keeps this cache bounded on this
              host; worker nodes use the defaults.
            </p>
            <div className="space-y-2">
              <Label htmlFor="build_cache_max_gb">
                Maximum cache size (GB)
              </Label>
              <Input
                id="build_cache_max_gb"
                type="number"
                min={0}
                max={10000}
                {...register('build_limits.build_cache_max_gb', {
                  valueAsNumber: true,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Least recently used cache that no build is using is removed
                until the cache fits. 0 turns the size cap off. Default 10.
              </p>
              {errors.build_limits?.build_cache_max_gb && (
                <p className="text-xs text-destructive">
                  {errors.build_limits.build_cache_max_gb.message}
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="build_cache_max_unused_days">
                Remove cache unused for (days)
              </Label>
              <Input
                id="build_cache_max_unused_days"
                type="number"
                min={1}
                max={365}
                {...register('build_limits.build_cache_max_unused_days', {
                  valueAsNumber: true,
                })}
              />
              <p className="text-xs text-muted-foreground">
                Applies whatever the cache size. Min 1, max 365. Default 7.
              </p>
              {errors.build_limits?.build_cache_max_unused_days && (
                <p className="text-xs text-destructive">
                  {errors.build_limits.build_cache_max_unused_days.message}
                </p>
              )}
            </div>
          </SettingsGroup>
        </div>

        <SaveRow sticky dirty={isDirty} saving={isSubmitting} />
      </form>
      <div className="max-w-5xl">
        <CiBuilderPanel />
      </div>
    </div>
  )
}
