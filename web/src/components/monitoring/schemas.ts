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

export const digestSectionsSchema = z.object({
  performance: z.boolean(),
  deployments: z.boolean(),
  errors: z.boolean(),
  funnels: z.boolean(),
  security: z.boolean(),
  resources: z.boolean(),
})

export const weeklyDigestSchema = z.object({
  weeklyDigestEnabled: z.boolean(),
  digestSendDay: z.enum(['monday', 'friday', 'sunday']),
  digestSendTime: z
    .string()
    .regex(/^([0-1][0-9]|2[0-3]):[0-5][0-9]$/, 'Invalid time format (HH:MM)'),
  digestSections: digestSectionsSchema,
})

/**
 * Schema for notification provider configuration
 * Supports both Slack and Email providers with comprehensive SMTP settings
 */
export const providerSchema = z
  .object({
    name: z.string().min(1, 'Name is required'),
    provider_type: z.enum(['email', 'slack']),
    config: z.object({
      // Slack config
      webhook_url: z.string().optional(),
      channel: z.string().optional(),
      slack_username: z.string().optional(),

      // Email config
      smtp_host: z.string().optional(),
      smtp_port: z.number().min(1).max(65535).optional(),
      use_credentials: z.boolean().optional(),
      smtp_username: z.string().optional(),
      password: z.string().optional(),
      from_name: z.string().optional(),
      from_address: z.string().optional(),
      to_addresses: z.array(z.string()).optional(),
      tls_mode: z.enum(['None', 'Starttls', 'Tls']).optional(),
      starttls_required: z.boolean().optional(),
      accept_invalid_certs: z.boolean().optional(),
    }),
  })
  .refine(
    (data) => {
      // Validate based on provider type
      if (data.provider_type === 'slack') {
        // Validate webhook URL only for Slack
        const url = data.config.webhook_url
        if (!url || url === '') {
          return false
        }
        // Check if it's a valid URL
        try {
          new URL(url)
          return true
        } catch {
          return false
        }
      }
      // Email provider validation
      if (data.provider_type === 'email') {
        const { smtp_host, smtp_port, from_address, to_addresses } = data.config

        // Check required email fields
        if (!smtp_host || !smtp_port || !from_address || !to_addresses || to_addresses.length === 0) {
          return false
        }

        // Validate from_address is a valid email
        const emailRegex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/
        if (!emailRegex.test(from_address)) {
          return false
        }

        // Validate all to_addresses are valid emails
        for (const email of to_addresses) {
          if (!emailRegex.test(email)) {
            return false
          }
        }

        return true
      }
      return false
    },
    {
      message: 'Please fill in all required fields for the selected provider type',
      path: ['config'],
    }
  )

export type ProjectAlertsFormData = z.infer<typeof projectAlertsSchema>
export type DomainAlertsFormData = z.infer<typeof domainAlertsSchema>
export type BackupAlertsFormData = z.infer<typeof backupAlertsSchema>
export type RouteAlertsFormData = z.infer<typeof routeAlertsSchema>
export type NotificationSettingsFormData = z.infer<
  typeof notificationSettingsSchema
>
export type DigestSectionsFormData = z.infer<typeof digestSectionsSchema>
export type WeeklyDigestFormData = z.infer<typeof weeklyDigestSchema>
export type ProviderFormData = z.infer<typeof providerSchema>
