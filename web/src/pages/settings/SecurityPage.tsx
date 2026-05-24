import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { AdminGateCard } from '@/components/settings/AdminGateCard'
import { SecuritySettings } from '@/components/settings/SecuritySettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertCircle, Loader2, Save } from 'lucide-react'
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
    },
  })

  const securityHeaders = useWatch({ control, name: 'security_headers' })
  const rateLimiting = useWatch({ control, name: 'rate_limiting' })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Security Headers' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Security Headers')

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
      <AdminGateCard />
      <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
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
