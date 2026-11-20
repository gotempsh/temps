// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader, SaveRow } from '@temps-sdk/ds'
import { SettingsLoadFailure } from '@/components/settings/SettingsLoadFailure'
import { isForbiddenRead } from '@/lib/read-failure'
import { MonitoringSettings } from '@/components/settings/MonitoringSettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettingsPage, useUpdateSettings } from '@/hooks/useSettings'
import { Loader2 } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import type { DiskSpaceAlertSettings } from '@/api/platformSettings'

interface DiskMonitoringFormData {
  disk_space_alert: DiskSpaceAlertSettings
}

export function DiskMonitoringPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error, refetch } = useSettingsPage()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    control,
    formState: { isDirty, isSubmitting },
    reset,
    setValue,
  } = useForm<DiskMonitoringFormData>({
    defaultValues: {
      disk_space_alert: {
        enabled: true,
        threshold_percent: 80,
        check_interval_seconds: 300,
        monitor_path: null,
      },
    },
  })

  const diskSpaceAlert = useWatch({ control, name: 'disk_space_alert' })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Disk monitoring' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Disk monitoring')

  useEffect(() => {
    if (settings) {
      reset({
        disk_space_alert: settings.disk_space_alert || {
          enabled: true,
          threshold_percent: 80,
          check_interval_seconds: 300,
          monitor_path: null,
        },
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: DiskMonitoringFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Disk monitoring settings saved')
    } catch {
      // useUpdateSettings already toasts this failure, with the server's reason.
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
      <SettingsLoadFailure
        error={error}
        onRetry={isForbiddenRead(error) ? undefined : () => void refetch()}
      />
    )
  }

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
      <PageHeader title="Disk monitoring" />
      <MonitoringSettings
        control={control}
        register={register}
        setValue={setValue}
        diskSpaceAlert={diskSpaceAlert}
      />
      <SaveRow sticky dirty={isDirty} saving={isSubmitting} />
    </form>
  )
}
