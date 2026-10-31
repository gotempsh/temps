// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { SettingsSection } from '@/components/ui/settings-section'
import { RateLimitFields } from '@/components/settings/RateLimitFields'
import { RATE_LIMITING_DESCRIPTION } from '@/components/settings/rate-limiting'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { Gauge, Info, ListFilter, ShieldCheck } from 'lucide-react'
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
  trust_loopback_forwarded_ip: boolean
  /** Edited by SecurityPage itself, like the networks below. */
  loopback_ja4_header: string
  /** Edited by SecurityPage itself; carried here so both share one form. */
  trusted_private_networks_text: string
}

interface SecuritySettingsProps {
  control: Control<SecuritySettingsFormData>
  register: UseFormRegister<SecuritySettingsFormData>
  setValue: UseFormSetValue<SecuritySettingsFormData>
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
  return (
    // Follows other groups on the Security page: draw the rule its first
    // group drops.
    <div className="space-y-6 border-t pt-8">
      <SettingsSection
        title="Security headers"
        icon={ShieldCheck}
        description="Configure HTTP security headers for all deployments"
      >
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="security-headers-enabled">
                Enable security headers
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
                <Label htmlFor="security-preset">Security preset</Label>
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
                    <Label htmlFor="csp">Content security policy</Label>
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
        </div>
      </SettingsSection>

      <SettingsSection
        id="rate-limiting"
        title="Rate limiting"
        icon={Gauge}
        description={RATE_LIMITING_DESCRIPTION}
      >
        <RateLimitFields
          value={rateLimiting}
          onChange={(next) =>
            setValue('rate_limiting', next, { shouldDirty: true })
          }
        />
      </SettingsSection>

      <SettingsSection
        id="ip-access"
        title="IP access control"
        description="Allow or block access by IP address and network range"
        icon={ListFilter}
      >
        <IpAccessControl />
      </SettingsSection>
    </div>
  )
}
