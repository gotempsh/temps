// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type { StatusPageWebhookResponse } from '@/api/client'

export const MAX_WEBHOOK_URL_CHARS = 2048
export const MIN_PAGE_PASSWORD_CHARS = 8
export const MAX_PAGE_PASSWORD_CHARS = 256

/** Why `value` cannot be a webhook URL, or null. The server checks the address too. */
export function webhookUrlProblem(value: string): string | null {
  const url = value.trim()
  if (!url) return 'Enter the URL to post events to'
  if (url.length > MAX_WEBHOOK_URL_CHARS) {
    return `At most ${MAX_WEBHOOK_URL_CHARS} characters`
  }
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return 'Enter a full URL, like https://hooks.example.com/status'
  }
  if (parsed.protocol !== 'https:') {
    return 'Use https://, so events and signatures are not sent in the clear'
  }
  if (parsed.username || parsed.password) {
    return 'Leave credentials out of the URL; verify the signature instead'
  }
  if (parsed.hash) return 'Remove the #fragment'
  return null
}

export const webhookSchema = z.object({
  url: z.string().superRefine((value, ctx) => {
    const problem = webhookUrlProblem(value)
    if (problem) ctx.addIssue({ code: 'custom', message: problem })
  }),
})
export type WebhookValues = z.infer<typeof webhookSchema>

/** One line on how deliveries to a webhook are going. */
export function deliveryState(webhook: StatusPageWebhookResponse): {
  label: string
  tone: 'success' | 'warning' | 'neutral'
  detail: string | null
} {
  if (!webhook.enabled) {
    return { label: 'Paused', tone: 'neutral', detail: null }
  }
  if (webhook.last_failure_reason) {
    return {
      label: 'Failing',
      tone: 'warning',
      detail: webhook.last_failure_reason,
    }
  }
  if (webhook.last_success_at) {
    return { label: 'Delivering', tone: 'success', detail: null }
  }
  return { label: 'No event yet', tone: 'neutral', detail: null }
}

export const pagePasswordSchema = z.object({
  password: z
    .string()
    .min(
      MIN_PAGE_PASSWORD_CHARS,
      `At least ${MIN_PAGE_PASSWORD_CHARS} characters`
    )
    .max(
      MAX_PAGE_PASSWORD_CHARS,
      `At most ${MAX_PAGE_PASSWORD_CHARS} characters`
    )
    // eslint-disable-next-line no-control-regex
    .refine((value) => !/[\u0000-\u001f\u007f]/.test(value), {
      message: 'No control characters',
    }),
})
export type PagePasswordValues = z.infer<typeof pagePasswordSchema>
