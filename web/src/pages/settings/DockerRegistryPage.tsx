// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { DockerRegistrySettings } from '@/components/settings/DockerRegistrySettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertCircle, Loader2, Save } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'

interface DockerRegistryFormData {
  docker_registry: {
    enabled: boolean
    registry_url: string | null
    username: string | null
    password: string | null
    tls_verify: boolean
    ca_certificate: string | null
  }
}

export function DockerRegistryPage() {
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
  } = useForm<DockerRegistryFormData>({
    defaultValues: {
      docker_registry: {
        enabled: false,
        registry_url: null,
        username: null,
        password: null,
        tls_verify: true,
        ca_certificate: null,
      },
    },
  })

  const dockerRegistry = useWatch({ control, name: 'docker_registry' })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Docker Registry' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Docker Registry')

  useEffect(() => {
    if (settings) {
      reset({
        docker_registry: settings.docker_registry || {
          enabled: false,
          registry_url: null,
          username: null,
          password: null,
          tls_verify: true,
          ca_certificate: null,
        },
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: DockerRegistryFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Docker registry settings saved')
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
      <DockerRegistrySettings
        control={control}
        register={register}
        setValue={setValue}
        dockerRegistry={dockerRegistry}
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
