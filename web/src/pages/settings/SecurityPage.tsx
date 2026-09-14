// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { SettingsSection } from '@/components/ui/settings-section'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import { AdminGateCard } from '@/components/settings/AdminGateCard'
import { SecuritySettings } from '@/components/settings/SecuritySettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertCircle, LockKeyhole, Loader2, Save } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import type {
  SecurityHeadersSettings as SecurityHeadersType,
  RateLimitSettings as RateLimitType,
} from '@/api/platformSettings'

interface SecurityFormData {
  security_headers: SecurityHeadersType
  rate_limiting: RateLimitType
  trust_loopback_forwarded_ip: boolean
}

export function SecurityPage() {
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
  } = useForm<SecurityFormData>({
    defaultValues: {
      security_headers: {
        enabled: true,
        preset: 'moderate',
        content_security_policy: null,
        x_frame_options: 'SAMEORIGIN',
        x_content_type_options: 'nosniff',
        x_xss_protection: '1; mode=block',
        strict_transport_security: 'max-age=31536000; includeSubDomains',
        referrer_policy: 'strict-origin-when-cross-origin',
        permissions_policy: null,
      },
      rate_limiting: {
        enabled: false,
        max_requests_per_minute: 60,
        max_requests_per_hour: 1000,
        whitelist_ips: [],
        blacklist_ips: [],
      },
      trust_loopback_forwarded_ip: false,
    },
  })

  const securityHeaders = useWatch({ control, name: 'security_headers' })
  const rateLimiting = useWatch({ control, name: 'rate_limiting' })
  const trustLoopbackForwardedIp = useWatch({
    control,
    name: 'trust_loopback_forwarded_ip',
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Security' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Security')

  useEffect(() => {
    if (settings) {
      reset({
        security_headers: settings.security_headers || {
          enabled: true,
          preset: 'moderate',
          content_security_policy: null,
          x_frame_options: 'SAMEORIGIN',
          x_content_type_options: 'nosniff',
          x_xss_protection: '1; mode=block',
          strict_transport_security: 'max-age=31536000; includeSubDomains',
          referrer_policy: 'strict-origin-when-cross-origin',
          permissions_policy: null,
        },
        rate_limiting: settings.rate_limiting || {
          enabled: false,
          max_requests_per_minute: 60,
          max_requests_per_hour: 1000,
          whitelist_ips: [],
          blacklist_ips: [],
        },
        trust_loopback_forwarded_ip: settings.trust_loopback_forwarded_ip,
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: SecurityFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Security settings saved')
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
    <div className="space-y-6">
      <SettingsSection
        title="Admin access gate"
        description="Require additional verification for sensitive administrative actions"
        icon={LockKeyhole}
      >
        <AdminGateCard />
      </SettingsSection>
      <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
        <SettingsSection
          title="Proxy client IP"
          description="Choose when Temps trusts forwarding headers from a local reverse proxy"
          icon={LockKeyhole}
        >
          <div className="space-y-3">
            <div className="flex items-center justify-between gap-4">
              <Label htmlFor="trust-loopback-forwarded-ip">
                Trust forwarded client IP from loopback
              </Label>
              <Switch
                id="trust-loopback-forwarded-ip"
                checked={trustLoopbackForwardedIp}
                onCheckedChange={(checked) =>
                  setValue('trust_loopback_forwarded_ip', checked, {
                    shouldDirty: true,
                  })
                }
              />
            </div>
            <p className="text-sm text-muted-foreground">
              Enable only when your reverse proxy connects to Temps over
              loopback and overwrites X-Real-IP and either overwrites
              X-Forwarded-For with the actual client address or appends it as
              the final entry. Otherwise, clients may spoof their IP in
              analytics and IP-based controls. Changes reach proxy processes
              within a few seconds.
            </p>
          </div>
        </SettingsSection>
        <SecuritySettings
          control={control}
          register={register}
          setValue={setValue}
          securityHeaders={securityHeaders}
          rateLimiting={rateLimiting}
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
    </div>
  )
}
