// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader, SaveRow } from '@temps-sdk/ds'
import { SettingsLoadFailure } from '@/components/settings/SettingsLoadFailure'
import { isForbiddenRead } from '@/lib/read-failure'
import { DockerRegistrySettings } from '@/components/settings/DockerRegistrySettings'
import { RegistryMirrorSettings } from '@/components/settings/RegistryMirrorSettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettingsPage, useUpdateSettings } from '@/hooks/useSettings'
import { Loader2 } from 'lucide-react'
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
  registry_mirror_prefix: string | null
}

export function DockerRegistryPage() {
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
      registry_mirror_prefix: null,
    },
  })

  const dockerRegistry = useWatch({ control, name: 'docker_registry' })
  const registryMirrorPrefix = useWatch({
    control,
    name: 'registry_mirror_prefix',
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Docker registry' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Docker registry')

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
        registry_mirror_prefix: settings.registry_mirror_prefix ?? null,
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: DockerRegistryFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Docker registry settings saved')
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
      <PageHeader title="Docker registry" />
      <DockerRegistrySettings
        control={control}
        register={register}
        setValue={setValue}
        dockerRegistry={dockerRegistry}
      />
      <RegistryMirrorSettings
        prefixField={register('registry_mirror_prefix')}
        currentPrefix={registryMirrorPrefix}
      />
      <SaveRow sticky dirty={isDirty} saving={isSubmitting} />
    </form>
  )
}
