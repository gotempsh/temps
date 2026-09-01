// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { MonitoringSettings } from '@/components/settings/MonitoringSettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertCircle, Loader2, Save } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import type { DiskSpaceAlertSettings } from '@/api/platformSettings'

interface DiskMonitoringFormData {
  disk_space_alert: DiskSpaceAlertSettings
}

export function DiskMonitoringPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
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
      { label: 'Disk Monitoring' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Disk Monitoring')

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
      toast.error('Failed to save settings')
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
      <MonitoringSettings
        control={control}
        register={register}
        setValue={setValue}
        diskSpaceAlert={diskSpaceAlert}
      />
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
