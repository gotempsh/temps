// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader, SaveRow, SettingsSection } from '@temps-sdk/ds'
import { SettingsFormSkeleton } from '@/components/settings/SettingsFormSkeleton'
import { SettingsLoadFailure } from '@/components/settings/SettingsLoadFailure'
import { RateLimitFields } from '@/components/settings/RateLimitFields'
import {
  DEFAULT_RATE_LIMITING,
  RATE_LIMITING_DESCRIPTION,
} from '@/components/settings/rate-limiting'
import { isForbiddenRead } from '@/lib/read-failure'

import { IpAccessControl } from '@/components/settings/IpAccessControl'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettingsPage, useUpdateSettings } from '@/hooks/useSettings'
import { Gauge, ListFilter } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import type { RateLimitSettings } from '@/api/platformSettings'

interface RateLimitingFormData {
  rate_limiting: RateLimitSettings
}

export function RateLimitingPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error, refetch } = useSettingsPage()
  const updateSettings = useUpdateSettings()

  const {
    handleSubmit,
    control,
    formState: { isDirty, isSubmitting },
    reset,
    setValue,
  } = useForm<RateLimitingFormData>({
    defaultValues: { rate_limiting: DEFAULT_RATE_LIMITING },
  })

  const rateLimiting = useWatch({ control, name: 'rate_limiting' })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Rate limiting' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Rate limiting')

  useEffect(() => {
    if (settings) {
      reset({
        rate_limiting: settings.rate_limiting || DEFAULT_RATE_LIMITING,
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: RateLimitingFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Rate limiting settings saved')
    } catch {
      // useUpdateSettings already toasts this failure, with the server's reason.
    }
  }

  if (isLoading) {
    return <SettingsFormSkeleton label="Loading rate limiting settings" />
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
      <PageHeader title="Rate limiting" />
      <SettingsSection
        title="Requests per IP"
        description={RATE_LIMITING_DESCRIPTION}
        icon={Gauge}
      >
        <RateLimitFields
          value={rateLimiting}
          onChange={(next) =>
            setValue('rate_limiting', next, { shouldDirty: true })
          }
        />
      </SettingsSection>

      <SaveRow dirty={isDirty} saving={isSubmitting} />

      {/* IP access control saves through its own API. */}
      <SettingsSection
        id="ip-access"
        title="IP access control"
        description="Allow or block access by IP address and network range"
        icon={ListFilter}
      >
        <IpAccessControl />
      </SettingsSection>
    </form>
  )
}
