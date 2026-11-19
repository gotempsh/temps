// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

export const projectAlertsSchema = z.object({
  deploymentFailures: z.boolean(),
  buildErrors: z.boolean(),
  runtimeErrors: z.object({
    enabled: z.boolean(),
    errorThreshold: z
      .number({ message: 'Error threshold is required' })
      .min(1)
      .max(1000),
    windowMinutes: z
      .number({ message: 'Time window is required' })
      .min(1)
      .max(60),
  }),
})

export const domainAlertsSchema = z.object({
  sslExpirationWarning: z.object({
    enabled: z.boolean(),
    daysBeforeExpiration: z
      .number({ message: 'Days before expiration is required' })
      .min(1)
      .max(90),
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
  // Slack delivery goes through a Slack notification provider; this is only
  // the stored preference flag; there is no webhook here to save.
  slack: z.object({
    enabled: z.boolean(),
  }),
  batchNotifications: z.boolean(),
  minimumSeverity: z.enum(['critical', 'warning', 'info']),
})

export const digestSectionsSchema = z.object({
  performance: z.boolean(),
  deployments: z.boolean(),
  errors: z.boolean(),
  funnels: z.boolean(),
  projects: z.boolean(),
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
 * Supports Slack, Email, Webhook, Cloudflare and Telegram providers
 */
const MASKED_VALUE = '***'

/** `<bot id>:<secret>`, as issued by @BotFather. Mirrors the server check. */
export const TELEGRAM_BOT_TOKEN_PATTERN = /^\d+:[A-Za-z0-9_-]+$/
/** A numeric user/group/channel id (`-100…`) or a public `@channelusername`. */
export const TELEGRAM_CHAT_ID_PATTERN = /^(-?\d+|@[A-Za-z0-9_]{1,64})$/

export const notificationRouteSeveritySchema = z.enum([
  'debug',
  'info',
  'warning',
  'error',
  'critical',
  'emergency',
])

/**
 * A number the form may leave empty. `NumberInput` reports an emptied box as
 * NaN (react-hook-form swaps `undefined` for the field's default, which would
 * put the old number back), and NaN is read here as "not provided".
 */
const optionalNumber = (schema: z.ZodNumber) =>
  z.union([schema, z.nan().transform(() => undefined)]).optional()

const buildProviderSchema = (allowMaskedValues: boolean) =>
  z
    .object({
      name: z.string().min(1, 'Name is required'),
      provider_type: z.enum([
        'email',
        'slack',
        'webhook',
        'cloudflare',
        'telegram',
      ]),
      config: z.object({
        // Slack config
        webhook_url: z.string().optional(),
        channel: z.string().optional(),
        slack_username: z.string().optional(),

        // Cloudflare config
        account_id: z.string().optional(),
        api_token: z.string().optional(),

        // Telegram config
        bot_token: z.string().optional(),
        chat_id: z.string().optional(),
        message_thread_id: optionalNumber(z.number().int().positive()),

        // Email config
        smtp_host: z.string().optional(),
        smtp_port: optionalNumber(z.number().min(1).max(65535)),
        use_credentials: z.boolean().optional(),
        smtp_username: z.string().optional(),
        password: z.string().optional(),
        from_name: z.string().optional(),
        from_address: z.string().optional(),
        to_addresses: z.array(z.string()).optional(),
        tls_mode: z.enum(['None', 'Starttls', 'Tls']).optional(),
        starttls_required: z.boolean().optional(),
        accept_invalid_certs: z.boolean().optional(),

        // Webhook config
        url: z.string().optional(),
        method: z.enum(['POST', 'PUT', 'PATCH']).optional(),
        headers: z.record(z.string(), z.string()).optional(),
        timeout_secs: optionalNumber(z.number().min(1).max(300)),
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
          if (allowMaskedValues && url === MASKED_VALUE) {
            return true
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
          const { smtp_host, smtp_port, from_address, to_addresses } =
            data.config

          // Check required email fields
          if (
            !smtp_host ||
            !smtp_port ||
            !from_address ||
            !to_addresses ||
            to_addresses.length === 0
          ) {
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
        // Webhook provider validation
        if (data.provider_type === 'webhook') {
          const { url } = data.config
          if (!url || url === '') {
            return false
          }
          if (allowMaskedValues && url === MASKED_VALUE) {
            return true
          }
          // Check if it's a valid URL
          try {
            const parsedUrl = new URL(url)
            // Only allow http and https protocols
            if (!['http:', 'https:'].includes(parsedUrl.protocol)) {
              return false
            }
            return true
          } catch {
            return false
          }
        }
        // Cloudflare provider validation
        if (data.provider_type === 'cloudflare') {
          const { account_id, api_token, from_address, to_addresses } =
            data.config

          if (
            !account_id ||
            !api_token ||
            !from_address ||
            !to_addresses ||
            to_addresses.length === 0
          ) {
            return false
          }

          const emailRegex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/
          if (!emailRegex.test(from_address)) {
            return false
          }
          for (const email of to_addresses) {
            if (!emailRegex.test(email)) {
              return false
            }
          }
          return true
        }
        // Telegram reports field-level problems in the superRefine below.
        if (data.provider_type === 'telegram') {
          return true
        }
        return false
      },
      {
        message:
          'Please fill in all required fields for the selected provider type',
        path: ['config'],
      }
    )
    .superRefine((data, ctx) => {
      if (data.provider_type !== 'telegram') return
      const botToken = data.config.bot_token?.trim() ?? ''
      const chatId = data.config.chat_id?.trim() ?? ''
      const keepsStoredToken = allowMaskedValues && botToken === MASKED_VALUE
      if (!botToken) {
        ctx.addIssue({
          code: 'custom',
          message: 'Bot token is required. Create a bot with @BotFather.',
          path: ['config', 'bot_token'],
        })
      } else if (
        !keepsStoredToken &&
        !TELEGRAM_BOT_TOKEN_PATTERN.test(botToken)
      ) {
        ctx.addIssue({
          code: 'custom',
          message:
            'Bot token must look like 123456789:AA… as issued by @BotFather.',
          path: ['config', 'bot_token'],
        })
      }
      if (!chatId) {
        ctx.addIssue({
          code: 'custom',
          message: 'Chat ID is required.',
          path: ['config', 'chat_id'],
        })
      } else if (!TELEGRAM_CHAT_ID_PATTERN.test(chatId)) {
        ctx.addIssue({
          code: 'custom',
          message:
            'Use a numeric chat ID (e.g. -1001234567890) or a public @channelusername.',
          path: ['config', 'chat_id'],
        })
      }
    })

export const providerSchema = buildProviderSchema(false)

export const providerUpdateSchema = buildProviderSchema(true)

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

/**
 * The stored Telegram config for a validated form. Sent through the generic
 * provider endpoints, which validate it server-side as well. A masked `***`
 * token on edit keeps the stored token.
 */
export function telegramConfigFromForm(config: ProviderFormData['config']) {
  return {
    bot_token: config.bot_token?.trim() ?? '',
    chat_id: config.chat_id?.trim() ?? '',
    message_thread_id: config.message_thread_id ?? null,
  }
}

/**
 * The edit form's Telegram fields for a stored config. The generic provider
 * endpoints store the config as submitted, and the server accepts `chat_id` as
 * a JSON integer, so it can come back as a number; the form needs the string
 * form or an unrelated edit (e.g. a rename) fails validation.
 */
export function telegramFormFieldsFromConfig(config: Record<string, unknown>) {
  const chatId = config.chat_id
  const threadId = config.message_thread_id
  return {
    bot_token: typeof config.bot_token === 'string' ? config.bot_token : '',
    chat_id:
      typeof chatId === 'string' || typeof chatId === 'number'
        ? String(chatId)
        : '',
    message_thread_id:
      typeof threadId === 'number' && Number.isSafeInteger(threadId)
        ? threadId
        : undefined,
  }
}
