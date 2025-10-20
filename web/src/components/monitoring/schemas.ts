import { z } from 'zod'

export const projectAlertsSchema = z.object({
  deploymentFailures: z.boolean(),
  buildErrors: z.boolean(),
  runtimeErrors: z.object({
    enabled: z.boolean(),
    errorThreshold: z.number().min(1).max(1000),
    windowMinutes: z.number().min(1).max(60),
  }),
})

export const domainAlertsSchema = z.object({
  sslExpirationWarning: z.object({
    enabled: z.boolean(),
    daysBeforeExpiration: z.number().min(1).max(90),
  }),
  domainExpirationWarning: z.boolean(),
  dnsConfigurationChanges: z.boolean(),
})

export const backupAlertsSchema = z.object({
  backupFailure: z.boolean(),
  s3ConnectionIssues: z.boolean(),
  retentionViolations: z.boolean(),
  backupSuccess: z.boolean(),
})

export const routeAlertsSchema = z.object({
  routeDowntime: z.boolean(),
  loadBalancerIssues: z.boolean(),
})

export const notificationSettingsSchema = z.object({
  email: z.boolean(),
  slack: z.object({
    enabled: z.boolean(),
    webhook: z.string().url().optional().or(z.literal('')),
  }),
  batchNotifications: z.boolean(),
  minimumSeverity: z.enum(['critical', 'warning', 'info']),
})

/**
 * Schema for notification provider configuration
 * Supports both Slack and Email providers with comprehensive SMTP settings
 */
export const providerSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  provider_type: z.enum(['email', 'slack']),
  config: z
    .object({
      // Slack config
      webhook_url: z.string().url('Invalid webhook URL').optional(),
      channel: z.string().optional(),
      slack_username: z.string().optional(),

      // Email config
      smtp_host: z.string().optional(),
      smtp_port: z.number().min(1).max(65535).optional(),
      use_credentials: z.boolean().optional(),
      smtp_username: z.string().optional(),
      password: z.string().optional(),
      from_name: z.string().optional(),
      from_address: z.string().email('Invalid from address').optional(),
      to_addresses: z
        .array(z.string().email('Invalid email address'))
        .optional(),
      tls_mode: z.enum(['None', 'Starttls', 'Tls']).optional(),
      starttls_required: z.boolean().optional(),
      accept_invalid_certs: z.boolean().optional(),
    })
    .refine((data) => {
      if (data.webhook_url) return true
      // Username and password are now optional for SMTP
      if (
        data.smtp_host &&
        data.smtp_port &&
        data.from_address &&
        data.to_addresses
      )
        return true
      return false
    }, 'Please fill in all required fields for the selected provider type'),
})

export type ProjectAlertsFormData = z.infer<typeof projectAlertsSchema>
export type DomainAlertsFormData = z.infer<typeof domainAlertsSchema>
export type BackupAlertsFormData = z.infer<typeof backupAlertsSchema>
export type RouteAlertsFormData = z.infer<typeof routeAlertsSchema>
export type NotificationSettingsFormData = z.infer<
  typeof notificationSettingsSchema
>
export type ProviderFormData = z.infer<typeof providerSchema>
