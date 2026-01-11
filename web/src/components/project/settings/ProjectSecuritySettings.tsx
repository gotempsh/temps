import { ProjectResponse } from '@/api/client'
import {
  updateProjectDeploymentConfigMutation,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { InfoIcon, Shield } from 'lucide-react'
import { useForm, Controller, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { useMutation } from '@tanstack/react-query'

interface ProjectSecuritySettingsProps {
  project: ProjectResponse
  refetch: () => void
}

interface SecurityHeadersConfig {
  preset?: string
  contentSecurityPolicy?: string
  xFrameOptions?: string
  strictTransportSecurity?: string
  referrerPolicy?: string
}

interface RateLimitConfig {
  maxRequestsPerMinute?: number
  maxRequestsPerHour?: number
  whitelistIps?: string[]
  blacklistIps?: string[]
}

interface SecurityConfig {
  enabled?: boolean
  headers?: SecurityHeadersConfig
  rateLimiting?: RateLimitConfig
}

interface FormData {
  security: SecurityConfig
  attack_mode?: boolean
}

export function ProjectSecuritySettings({
  project,
  refetch,
}: ProjectSecuritySettingsProps) {
  const updateDeploymentConfig = useMutation({
    ...updateProjectDeploymentConfigMutation(),
    meta: {
      errorTitle: 'Failed to update security configuration',
    },
  })

  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update attack mode',
    },
  })

  const {
    control,
    register,
    handleSubmit,
    setValue,
    watch,
    formState: { isDirty, isSubmitting },
  } = useForm<FormData>({
    defaultValues: {
      attack_mode: project.attack_mode ?? false,
      security: {
        enabled: project.deployment_config?.security?.enabled ?? undefined,
        headers: {
          preset:
            project.deployment_config?.security?.headers?.preset ?? undefined,
          contentSecurityPolicy:
            project.deployment_config?.security?.headers
              ?.contentSecurityPolicy ?? undefined,
          xFrameOptions:
            project.deployment_config?.security?.headers?.xFrameOptions ??
            undefined,
          strictTransportSecurity:
            project.deployment_config?.security?.headers
              ?.strictTransportSecurity ?? undefined,
          referrerPolicy:
            project.deployment_config?.security?.headers?.referrerPolicy ??
            undefined,
        },
        rateLimiting: {
          maxRequestsPerMinute:
            project.deployment_config?.security?.rateLimiting
              ?.maxRequestsPerMinute ?? undefined,
          maxRequestsPerHour:
            project.deployment_config?.security?.rateLimiting
              ?.maxRequestsPerHour ?? undefined,
          whitelistIps:
            project.deployment_config?.security?.rateLimiting?.whitelistIps ??
            [],
          blacklistIps:
            project.deployment_config?.security?.rateLimiting?.blacklistIps ??
            [],
        },
      },
    },
  })

  const securityConfig = useWatch({ control, name: 'security' })

  const onSubmit = async (data: FormData) => {
    if (!project?.id) return

    try {
      // Check if attack_mode has changed
      const attackModeChanged = data.attack_mode !== project.attack_mode

      // Update attack mode if changed
      if (attackModeChanged) {
        await toast.promise(
          updateProjectSettings.mutateAsync({
            path: { project_id: project.id },
            body: {
              attack_mode: data.attack_mode,
            },
          }),
          {
            loading: 'Updating attack mode...',
            success: 'Attack mode updated successfully',
            error: 'Failed to update attack mode',
          }
        )
      }

      // Update deployment config (security headers and rate limiting)
      await toast.promise(
        updateDeploymentConfig.mutateAsync({
          path: { project_id: project.id },
          body: {
            security: data.security,
          },
        }),
        {
          loading: 'Updating security configuration...',
          success: 'Security configuration updated successfully',
          error: 'Failed to update security configuration',
        }
      )

      refetch()
    } catch (error) {
      // Error already handled by toast.promise
      console.error('Failed to update settings:', error)
    }
  }

  const handleAddWhitelistIp = () => {
    const current = securityConfig?.rateLimiting?.whitelistIps || []
    setValue('security.rateLimiting.whitelistIps', [...current, ''], {
      shouldDirty: true,
    })
  }

  const handleRemoveWhitelistIp = (index: number) => {
    const current = securityConfig?.rateLimiting?.whitelistIps || []
    setValue(
      'security.rateLimiting.whitelistIps',
      current.filter((_, i) => i !== index),
      { shouldDirty: true }
    )
  }

  const handleUpdateWhitelistIp = (index: number, value: string) => {
    const current = securityConfig?.rateLimiting?.whitelistIps || []
    const updated = [...current]
    updated[index] = value
    setValue('security.rateLimiting.whitelistIps', updated, {
      shouldDirty: true,
    })
  }

  const handleAddBlacklistIp = () => {
    const current = securityConfig?.rateLimiting?.blacklistIps || []
    setValue('security.rateLimiting.blacklistIps', [...current, ''], {
      shouldDirty: true,
    })
  }

  const handleRemoveBlacklistIp = (index: number) => {
    const current = securityConfig?.rateLimiting?.blacklistIps || []
    setValue(
      'security.rateLimiting.blacklistIps',
      current.filter((_, i) => i !== index),
      { shouldDirty: true }
    )
  }

  const handleUpdateBlacklistIp = (index: number, value: string) => {
    const current = securityConfig?.rateLimiting?.blacklistIps || []
    const updated = [...current]
    updated[index] = value
    setValue('security.rateLimiting.blacklistIps', updated, {
      shouldDirty: true,
    })
  }

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
      <Alert>
        <InfoIcon className="h-4 w-4" />
        <AlertTitle>Configuration Inheritance</AlertTitle>
        <AlertDescription>
          Project-level security settings override global settings. Leave fields
          empty to inherit from global configuration.
        </AlertDescription>
      </Alert>

      {/* Attack Mode Card */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Attack Mode
          </CardTitle>
          <CardDescription>
            Enable CAPTCHA protection to defend against DDoS attacks and bot
            traffic
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="attack-mode">Enable Attack Mode</Label>
              <p className="text-sm text-muted-foreground">
                Require CAPTCHA verification for all visitors to this project
              </p>
            </div>
            <Switch
              id="attack-mode"
              checked={watch('attack_mode') ?? false}
              onCheckedChange={(checked) =>
                setValue('attack_mode', checked, { shouldDirty: true })
              }
            />
          </div>
          {watch('attack_mode') && (
            <>
              <Separator />
              <Alert>
                <InfoIcon className="h-4 w-4" />
                <AlertTitle>Attack Mode Active</AlertTitle>
                <AlertDescription>
                  All visitors will be required to complete a CAPTCHA challenge
                  before accessing your application. Sessions are valid for 24
                  hours.
                </AlertDescription>
              </Alert>
            </>
          )}
        </CardContent>
        <CardFooter>
          <Button
            type="submit"
            disabled={
              !isDirty || isSubmitting || updateDeploymentConfig.isPending
            }
          >
            Save Attack Mode Settings
          </Button>
        </CardFooter>
      </Card>

      {/* Security Headers Card */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Security Headers
          </CardTitle>
          <CardDescription>
            Configure HTTP security headers for this project. Overrides global
            settings.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="security-enabled">Enable Security Headers</Label>
              <p className="text-sm text-muted-foreground">
                Apply security headers to HTTP responses
              </p>
            </div>
            <Switch
              id="security-enabled"
              checked={securityConfig?.enabled ?? false}
              onCheckedChange={(checked) =>
                setValue('security.enabled', checked, { shouldDirty: true })
              }
            />
          </div>

          {securityConfig?.enabled && (
            <>
              <Separator />
              <div className="space-y-2">
                <Label htmlFor="security-preset">Security Preset</Label>
                <Controller
                  name="security.headers.preset"
                  control={control}
                  render={({ field }) => (
                    <Select
                      value={field.value || 'inherit'}
                      onValueChange={(value) => {
                        field.onChange(value === 'inherit' ? undefined : value)
                      }}
                    >
                      <SelectTrigger id="security-preset">
                        <SelectValue placeholder="Inherit from global" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="inherit">
                          Inherit from global
                        </SelectItem>
                        <SelectItem value="strict">
                          Strict - Maximum security
                        </SelectItem>
                        <SelectItem value="moderate">
                          Moderate - Balanced security
                        </SelectItem>
                        <SelectItem value="permissive">
                          Permissive - Development friendly
                        </SelectItem>
                        <SelectItem value="custom">
                          Custom - Manual configuration
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  )}
                />
                <p className="text-sm text-muted-foreground">
                  Choose a preset or inherit from global settings
                </p>
              </div>

              {securityConfig?.headers?.preset === 'custom' && (
                <>
                  <div className="space-y-2">
                    <Label htmlFor="csp">Content Security Policy</Label>
                    <Input
                      id="csp"
                      placeholder="Inherit from global or enter custom CSP"
                      {...register('security.headers.contentSecurityPolicy')}
                    />
                  </div>

                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    <div className="space-y-2">
                      <Label htmlFor="x-frame-options">X-Frame-Options</Label>
                      <Input
                        id="x-frame-options"
                        placeholder="DENY"
                        {...register('security.headers.xFrameOptions')}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="hsts">Strict-Transport-Security</Label>
                      <Input
                        id="hsts"
                        placeholder="max-age=31536000; includeSubDomains"
                        {...register(
                          'security.headers.strictTransportSecurity'
                        )}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="referrer-policy">Referrer-Policy</Label>
                      <Input
                        id="referrer-policy"
                        placeholder="strict-origin-when-cross-origin"
                        {...register('security.headers.referrerPolicy')}
                      />
                    </div>
                  </div>
                </>
              )}
            </>
          )}
        </CardContent>
        <CardFooter>
          <Button
            type="submit"
            disabled={
              !isDirty || isSubmitting || updateDeploymentConfig.isPending
            }
          >
            Save Security Configuration
          </Button>
        </CardFooter>
      </Card>

      {/* Rate Limiting Card */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Rate Limiting
          </CardTitle>
          <CardDescription>
            Configure rate limiting for this project. Overrides global settings.
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
              checked={securityConfig?.enabled ?? false}
              onCheckedChange={(checked) =>
                setValue('security.enabled', checked, { shouldDirty: true })
              }
            />
          </div>

          {securityConfig?.enabled && (
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
                    placeholder="Inherit from global"
                    {...register('security.rateLimiting.maxRequestsPerMinute', {
                      valueAsNumber: true,
                    })}
                  />
                  <p className="text-sm text-muted-foreground">
                    Override global rate limit per minute
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
                    placeholder="Inherit from global"
                    {...register('security.rateLimiting.maxRequestsPerHour', {
                      valueAsNumber: true,
                    })}
                  />
                  <p className="text-sm text-muted-foreground">
                    Override global rate limit per hour
                  </p>
                </div>
              </div>

              <Separator />

              <div className="space-y-4">
                <div>
                  <Label>Whitelist IPs (Project-specific)</Label>
                  <p className="text-sm text-muted-foreground mb-2">
                    Additional IPs that bypass rate limiting for this project
                  </p>
                  <div className="space-y-2">
                    {(securityConfig?.rateLimiting?.whitelistIps || []).map(
                      (ip, index) => (
                        <div key={index} className="flex gap-2">
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
                            <Shield className="h-4 w-4" />
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
                      Add Whitelist IP
                    </Button>
                  </div>
                </div>

                <div>
                  <Label>Blacklist IPs (Project-specific)</Label>
                  <p className="text-sm text-muted-foreground mb-2">
                    Additional IPs to block for this project
                  </p>
                  <div className="space-y-2">
                    {(securityConfig?.rateLimiting?.blacklistIps || []).map(
                      (ip, index) => (
                        <div key={index} className="flex gap-2">
                          <Input
                            value={ip}
                            onChange={(e) =>
                              handleUpdateBlacklistIp(index, e.target.value)
                            }
                            placeholder="192.168.1.1 or 10.0.0.0/24"
                          />
                          <Button
                            type="button"
                            variant="outline"
                            size="icon"
                            onClick={() => handleRemoveBlacklistIp(index)}
                          >
                            <Shield className="h-4 w-4" />
                          </Button>
                        </div>
                      )
                    )}
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={handleAddBlacklistIp}
                    >
                      Add Blacklist IP
                    </Button>
                  </div>
                </div>
              </div>
            </>
          )}
        </CardContent>
      </Card>
    </form>
  )
}
