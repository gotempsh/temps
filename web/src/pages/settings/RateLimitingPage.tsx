// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { IpAccessControl } from '@/components/settings/IpAccessControl'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertCircle, Loader2, Plus, Save, Shield, Trash2 } from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import type { RateLimitSettings } from '@/api/platformSettings'

interface RateLimitingFormData {
  rate_limiting: RateLimitSettings
}

export function RateLimitingPage() {
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
  } = useForm<RateLimitingFormData>({
    defaultValues: {
      rate_limiting: {
        enabled: false,
        max_requests_per_minute: 60,
        max_requests_per_hour: 1000,
        whitelist_ips: [],
        blacklist_ips: [],
      },
    },
  })

  const rateLimiting = useWatch({ control, name: 'rate_limiting' })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Rate Limiting' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Rate Limiting')

  useEffect(() => {
    if (settings) {
      reset({
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

  const onSubmit = async (data: RateLimitingFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Rate limiting settings saved')
    } catch {
      toast.error('Failed to save settings')
    }
  }

  const handleAddWhitelistIp = () => {
    const current = rateLimiting?.whitelist_ips || []
    setValue('rate_limiting.whitelist_ips', [...current, ''], {
      shouldDirty: true,
    })
  }

  const handleRemoveWhitelistIp = (index: number) => {
    const current = rateLimiting?.whitelist_ips || []
    setValue(
      'rate_limiting.whitelist_ips',
      current.filter((_: string, i: number) => i !== index),
      { shouldDirty: true }
    )
  }

  const handleUpdateWhitelistIp = (index: number, value: string) => {
    const current = rateLimiting?.whitelist_ips || []
    const updated = [...current]
    updated[index] = value
    setValue('rate_limiting.whitelist_ips', updated, { shouldDirty: true })
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
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Rate Limiting
          </CardTitle>
          <CardDescription>
            Configure rate limiting to prevent abuse
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="rate-limiting-enabled">
                Enable Rate Limiting
              </Label>
              <p className="text-sm text-muted-foreground">
                Limit requests per IP address
              </p>
            </div>
            <Switch
              id="rate-limiting-enabled"
              checked={rateLimiting?.enabled}
              onCheckedChange={(checked) =>
                setValue('rate_limiting.enabled', checked, {
                  shouldDirty: true,
                })
              }
            />
          </div>

          {rateLimiting?.enabled && (
            <>
              <Separator />
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                <div className="space-y-2">
                  <Label htmlFor="max-requests-per-minute">
                    Max Requests Per Minute
                  </Label>
                  <Input
                    id="max-requests-per-minute"
                    type="number"
                    min="1"
                    placeholder="60"
                    {...register('rate_limiting.max_requests_per_minute', {
                      valueAsNumber: true,
                    })}
                  />
                  <p className="text-sm text-muted-foreground">
                    Maximum requests allowed per IP per minute
                  </p>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="max-requests-per-hour">
                    Max Requests Per Hour
                  </Label>
                  <Input
                    id="max-requests-per-hour"
                    type="number"
                    min="1"
                    placeholder="1000"
                    {...register('rate_limiting.max_requests_per_hour', {
                      valueAsNumber: true,
                    })}
                  />
                  <p className="text-sm text-muted-foreground">
                    Maximum requests allowed per IP per hour
                  </p>
                </div>
              </div>

              <Separator />

              <div>
                <Label>Whitelist IPs</Label>
                <p className="text-sm text-muted-foreground mb-2">
                  IPs that bypass rate limiting
                </p>
                <div className="space-y-2">
                  {(rateLimiting?.whitelist_ips || []).map(
                    (ip: string, index: number) => (
                      <div key={`whitelist-${index}`} className="flex gap-2">
                        <Input
                          value={ip}
                          onChange={(e) =>
                            handleUpdateWhitelistIp(index, e.target.value)
                          }
                          placeholder="192.168.1.1 or 10.0.0.0/24"
                        />
                        <Button
                          type="button"
                          variant="outline"
                          size="icon"
                          onClick={() => handleRemoveWhitelistIp(index)}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </div>
                    )
                  )}
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={handleAddWhitelistIp}
                  >
                    <Plus className="h-4 w-4 mr-2" />
                    Add Whitelist IP
                  </Button>
                </div>
              </div>
            </>
          )}
        </CardContent>
      </Card>

      {/* IP Access Control - Uses dedicated API */}
      <IpAccessControl />

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
