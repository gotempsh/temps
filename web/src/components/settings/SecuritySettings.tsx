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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { Trash2, Plus, Shield, Info } from 'lucide-react'
import {
  Control,
  Controller,
  UseFormRegister,
  UseFormSetValue,
} from 'react-hook-form'
import { IpAccessControl } from './IpAccessControl'

const SECURITY_PRESET_DESCRIPTIONS: Record<
  string,
  { title: string; description: string; details: string[] }
> = {
  strict: {
    title: '🔒 Maximum Security',
    description:
      'Most restrictive configuration for high-security production environments.',
    details: [
      'CSP: Very restrictive - only allows resources from same origin',
      'X-Frame-Options: DENY - prevents clickjacking attacks',
      'Referrer-Policy: no-referrer - minimal referrer information',
      'Permissions-Policy: Disables most browser features by default',
      'Best for: Production apps with full control over all resources',
    ],
  },
  moderate: {
    title: '⚖️ Balanced Security',
    description:
      'Balanced approach between security and functionality for most production apps.',
    details: [
      'CSP: Allows inline scripts with nonces/hashes and certain external domains',
      'X-Frame-Options: SAMEORIGIN - allows framing from same origin',
      'Referrer-Policy: strict-origin-when-cross-origin - balanced sharing',
      'Permissions-Policy: Selective feature restrictions',
      'Best for: Production apps with third-party integrations (analytics, CDNs)',
    ],
  },
  permissive: {
    title: '🛠️ Development Friendly',
    description:
      'Relaxed security headers for local development and testing environments.',
    details: [
      'CSP: Very permissive - allows unsafe-inline and unsafe-eval for debugging',
      'X-Frame-Options: May be disabled or set to SAMEORIGIN',
      'Allows hot-reload, eval() for debugging tools',
      'Easier integration with third-party development tools',
      'Best for: Local development and testing environments only',
    ],
  },
  custom: {
    title: '⚙️ Custom Configuration',
    description: 'Manually configure each security header with custom values.',
    details: [
      'Full control over each header value',
      'Define your own Content Security Policy',
      'Configure frame options, referrer policy, and more',
      'Best for: Specific security requirements or compliance needs (PCI-DSS, SOC 2)',
    ],
  },
  disabled: {
    title: '❌ Disabled',
    description:
      'No security headers applied - use only for debugging. Never use in production.',
    details: [
      'No HTTP security header protection',
      'Application runs without any security headers',
      'Vulnerable to XSS, clickjacking, and other attacks',
      '⚠️ Warning: Only for local development or debugging',
    ],
  },
}

export interface SecurityHeadersSettings {
  enabled: boolean
  preset: string
  content_security_policy: string | null
  x_frame_options: string
  x_content_type_options: string
  x_xss_protection: string
  strict_transport_security: string
  referrer_policy: string
  permissions_policy: string | null
}

export interface RateLimitSettings {
  enabled: boolean
  max_requests_per_minute: number
  max_requests_per_hour: number
  whitelist_ips: string[]
  blacklist_ips: string[]
}

export interface SecuritySettingsFormData {
  security_headers: SecurityHeadersSettings
  rate_limiting: RateLimitSettings
}

interface SecuritySettingsProps {
  control: Control<any>
  register: UseFormRegister<any>
  setValue: UseFormSetValue<any>
  securityHeaders: SecurityHeadersSettings | undefined
  rateLimiting: RateLimitSettings | undefined
}

export function SecuritySettings({
  control,
  register,
  setValue,
  securityHeaders,
  rateLimiting,
}: SecuritySettingsProps) {
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
      current.filter((_, i) => i !== index),
      { shouldDirty: true }
    )
  }

  const handleUpdateWhitelistIp = (index: number, value: string) => {
    const current = rateLimiting?.whitelist_ips || []
    const updated = [...current]
    updated[index] = value
    setValue('rate_limiting.whitelist_ips', updated, { shouldDirty: true })
  }

  return (
    <div className="space-y-6">
      {/* Security Headers Card */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Security Headers
          </CardTitle>
          <CardDescription>
            Configure HTTP security headers for all deployments
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="security-headers-enabled">
                Enable Security Headers
              </Label>
              <p className="text-sm text-muted-foreground">
                Apply security headers to HTTP responses
              </p>
            </div>
            <Switch
              id="security-headers-enabled"
              checked={securityHeaders?.enabled}
              onCheckedChange={(checked) =>
                setValue('security_headers.enabled', checked, {
                  shouldDirty: true,
                })
              }
            />
          </div>

          {securityHeaders?.enabled && (
            <>
              <Separator />
              <div className="space-y-2">
                <Label htmlFor="security-preset">Security Preset</Label>
                <Controller
                  name="security_headers.preset"
                  control={control}
                  render={({ field }) => (
                    <Select value={field.value} onValueChange={field.onChange}>
                      <SelectTrigger id="security-preset">
                        <SelectValue placeholder="Select preset" />
                      </SelectTrigger>
                      <SelectContent>
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
                        <SelectItem value="disabled">Disabled</SelectItem>
                      </SelectContent>
                    </Select>
                  )}
                />
                <p className="text-sm text-muted-foreground">
                  Choose a preset or customize individual headers
                </p>
              </div>

              {securityHeaders?.preset &&
                SECURITY_PRESET_DESCRIPTIONS[securityHeaders.preset] && (
                  <div className="rounded-lg border bg-muted/50 p-4 space-y-3">
                    <div className="flex items-start gap-2">
                      <Info className="h-5 w-5 text-muted-foreground mt-0.5 flex-shrink-0" />
                      <div className="space-y-2 flex-1">
                        <div>
                          <h4 className="font-semibold text-sm">
                            {
                              SECURITY_PRESET_DESCRIPTIONS[
                                securityHeaders.preset
                              ].title
                            }
                          </h4>
                          <p className="text-sm text-muted-foreground mt-1">
                            {
                              SECURITY_PRESET_DESCRIPTIONS[
                                securityHeaders.preset
                              ].description
                            }
                          </p>
                        </div>
                        <div className="space-y-1">
                          {SECURITY_PRESET_DESCRIPTIONS[
                            securityHeaders.preset
                          ].details.map((detail, index) => (
                            <div
                              key={index}
                              className="text-xs text-muted-foreground flex items-start gap-2"
                            >
                              <span className="text-primary mt-0.5">•</span>
                              <span>{detail}</span>
                            </div>
                          ))}
                        </div>
                      </div>
                    </div>
                  </div>
                )}

              {securityHeaders?.preset === 'custom' && (
                <>
                  <div className="space-y-2">
                    <Label htmlFor="csp">Content Security Policy</Label>
                    <Input
                      id="csp"
                      placeholder="default-src 'self'; script-src 'self'"
                      {...register('security_headers.content_security_policy')}
                    />
                    <p className="text-sm text-muted-foreground">
                      Control which resources can be loaded on your site
                    </p>
                  </div>

                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    <div className="space-y-2">
                      <Label htmlFor="x-frame-options">X-Frame-Options</Label>
                      <Input
                        id="x-frame-options"
                        placeholder="DENY"
                        {...register('security_headers.x_frame_options')}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="x-content-type-options">
                        X-Content-Type-Options
                      </Label>
                      <Input
                        id="x-content-type-options"
                        placeholder="nosniff"
                        {...register('security_headers.x_content_type_options')}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="x-xss-protection">X-XSS-Protection</Label>
                      <Input
                        id="x-xss-protection"
                        placeholder="1; mode=block"
                        {...register('security_headers.x_xss_protection')}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="referrer-policy">Referrer-Policy</Label>
                      <Input
                        id="referrer-policy"
                        placeholder="strict-origin-when-cross-origin"
                        {...register('security_headers.referrer_policy')}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="hsts">Strict-Transport-Security</Label>
                      <Input
                        id="hsts"
                        placeholder="max-age=31536000; includeSubDomains"
                        {...register(
                          'security_headers.strict_transport_security'
                        )}
                      />
                    </div>

                    <div className="space-y-2">
                      <Label htmlFor="permissions-policy">
                        Permissions-Policy
                      </Label>
                      <Input
                        id="permissions-policy"
                        placeholder="geolocation=(), microphone=()"
                        {...register('security_headers.permissions_policy')}
                      />
                    </div>
                  </div>
                </>
              )}
            </>
          )}
        </CardContent>
      </Card>

      {/* Rate Limiting Card */}
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
                  {(rateLimiting?.whitelist_ips || []).map((ip, index) => (
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
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  ))}
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
    </div>
  )
}
