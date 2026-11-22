// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  EmailDomainResponse,
  EmailProviderResponse,
  EmailStatsResponse,
} from '@/api/client'
import { isSmtpProvider } from '@/lib/email-smtp-domains'

export interface EmailSetupStep {
  id: 'provider' | 'domain' | 'send'
  title: string
  detail: string
  done: boolean
  action?: { label: string; to: string }
}

function providerStep(providers: EmailProviderResponse[]): EmailSetupStep {
  const [first] = providers
  if (!first) {
    return {
      id: 'provider',
      title: 'Connect a provider',
      detail:
        'AWS SES, Scaleway or any SMTP relay. Until you connect one, emails are captured here instead of delivered.',
      done: false,
      action: { label: 'Add provider', to: '/email/providers/new' },
    }
  }
  const others = providers.length - 1
  return {
    id: 'provider',
    title: 'Connect a provider',
    detail:
      others > 0
        ? `Connected ${first.name} and ${others} more`
        : `Connected ${first.name}`,
    done: true,
  }
}

function domainStep(
  domains: EmailDomainResponse[],
  providers: EmailProviderResponse[]
): EmailSetupStep {
  const title = 'Verify the domain you send from'
  const verified = domains.find((domain) => domain.status === 'verified')
  if (verified) {
    return {
      id: 'domain',
      title,
      detail: `${verified.domain} can send`,
      done: true,
    }
  }
  const [waiting] = domains
  if (!waiting) {
    return {
      id: 'domain',
      title,
      detail:
        'Add a domain, then publish the DNS records that prove you own it.',
      done: false,
      action: { label: 'Add domain', to: '/email/domains/new' },
    }
  }
  const to = `/email/domains/${waiting.id}`
  // Temps checks no DNS for SMTP domains: they only wait to be enabled.
  const provider = providers.find((p) => p.id === waiting.provider_id)
  if (isSmtpProvider(provider)) {
    return {
      id: 'domain',
      title,
      detail: `Sending from ${waiting.domain} is paused until you enable it.`,
      done: false,
      action: { label: 'Enable sending', to },
    }
  }
  return {
    id: 'domain',
    title,
    detail:
      waiting.status === 'failed'
        ? `${waiting.domain} failed verification. Check its DNS records and try again.`
        : `${waiting.domain} is waiting on its DNS records.`,
    done: false,
    action: { label: 'Open DNS records', to },
  }
}

function sendStep(stats: EmailStatsResponse): EmailSetupStep {
  const title = 'Send your first email'
  if (stats.sent > 0) {
    return {
      id: 'send',
      title,
      detail: `${stats.sent.toLocaleString()} sent`,
      done: true,
    }
  }
  return {
    id: 'send',
    title,
    detail:
      stats.failed > 0
        ? `${stats.failed.toLocaleString()} failed so far. Open one under Sent emails to see why.`
        : 'Call the API or the SDK from your code. Apps deployed on Temps also need their project authorized on the domain.',
    done: false,
    action: { label: 'See how to send', to: '/email?tab=sdk' },
  }
}

/** The path from nothing to a delivered email, derived from what exists. */
export function emailSetupSteps(input: {
  providers: EmailProviderResponse[]
  domains: EmailDomainResponse[]
  stats: EmailStatsResponse
}): EmailSetupStep[] {
  return [
    providerStep(input.providers),
    domainStep(input.domains, input.providers),
    sendStep(input.stats),
  ]
}

/** Which parts of the email overview to show below the setup checklist. */
export interface EmailOverviewSections {
  domains: boolean
  providers: boolean
  sent: boolean
  events: boolean
}

const ALL_SECTIONS: EmailOverviewSections = {
  domains: true,
  providers: true,
  sent: true,
  events: true,
}

/**
 * While setup is unfinished the checklist already asks for each missing piece,
 * so a section that would only repeat it as an empty state stays out; a
 * section with something in it (a provider, a domain, a captured email) still
 * shows, since that is what the user needs to manage. Once every step is done
 * the checklist leaves and every section shows, empty or not.
 */
export function emailOverviewSections(input: {
  providers: EmailProviderResponse[]
  domains: EmailDomainResponse[]
  stats: EmailStatsResponse
}): EmailOverviewSections {
  if (emailSetupSteps(input).every((step) => step.done)) return ALL_SECTIONS
  const anyEmail = input.stats.total > 0
  return {
    domains: input.domains.length > 0,
    providers: input.providers.length > 0,
    sent: anyEmail,
    events: anyEmail,
  }
}
