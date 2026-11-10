// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { AccessServiceResponse } from '@/api/client'
export const serviceProviders = [
  { id: 'github', name: 'GitHub', origin: 'https://api.github.com' },
  { id: 'gitlab', name: 'GitLab', origin: 'https://gitlab.com' },
  { id: 'openai', name: 'OpenAI', origin: 'https://api.openai.com' },
  { id: 'anthropic', name: 'Anthropic', origin: 'https://api.anthropic.com' },
  { id: 'stripe', name: 'Stripe', origin: 'https://api.stripe.com' },
  { id: 'resend', name: 'Resend', origin: 'https://api.resend.com' },
  { id: 'sendgrid', name: 'SendGrid', origin: 'https://api.sendgrid.com' },
  { id: 'vercel', name: 'Vercel', origin: 'https://api.vercel.com' },
  { id: 'netlify', name: 'Netlify', origin: 'https://api.netlify.com' },
  { id: 'mistral', name: 'Mistral AI', origin: 'https://api.mistral.ai' },
  { id: 'groq', name: 'Groq', origin: 'https://api.groq.com' },
] as const

export function serviceProviderName(
  service: Pick<AccessServiceResponse, 'policy'>
) {
  if (service.policy.github_repository) return 'GitHub'
  return (
    serviceProviders.find((p) => p.id === service.policy.provider)?.name ??
    'Custom HTTP service'
  )
}
