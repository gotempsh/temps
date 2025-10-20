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

export const providerSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  provider_type: z.enum(['email', 'slack']),
  enabled: z.boolean().default(true),
  config: z
    .object({
      webhook_url: z.string().url('Invalid webhook URL').optional(),
      email_address: z.string().email('Invalid email address').optional(),
    })
    .optional(),
})

export type ProjectAlertsFormData = z.infer<typeof projectAlertsSchema>
export type DomainAlertsFormData = z.infer<typeof domainAlertsSchema>
export type BackupAlertsFormData = z.infer<typeof backupAlertsSchema>
export type RouteAlertsFormData = z.infer<typeof routeAlertsSchema>
export type NotificationSettingsFormData = z.infer<
  typeof notificationSettingsSchema
>
export type ProviderFormData = z.infer<typeof providerSchema>
