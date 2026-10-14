// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  EmailDomainWithDnsResponse,
  EmailProviderResponse,
} from '@/api/client'
import { toast } from 'sonner'

/**
 * Temps checks no DNS for SMTP domains: they are verified at the upstream
 * provider. "Verify" on an SMTP domain only records that it may send (the
 * stored status gates sending), so there are no records to report on.
 */
export function isSmtpProvider<
  P extends Pick<EmailProviderResponse, 'provider_type'>,
>(provider: P | undefined): provider is P {
  return provider?.provider_type === 'smtp'
}

/** Toast for the result of enabling sending on an SMTP domain. */
export function smtpVerifyToast(
  data: EmailDomainWithDnsResponse,
  provider: Pick<EmailProviderResponse, 'name'>
) {
  if (data.domain.status === 'verified') {
    toast.success('Sending enabled', {
      description: `Emails from ${data.domain.domain} are now delivered through ${provider.name}.`,
    })
  } else {
    toast.error('Could not enable sending', {
      description:
        data.domain.verification_error ??
        `${data.domain.domain} is still ${data.domain.status}.`,
    })
  }
}
