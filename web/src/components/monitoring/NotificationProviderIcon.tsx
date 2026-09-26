// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Mail, Webhook } from 'lucide-react'
import { CloudflareIcon } from '@/components/icons/DnsProviderIcons'
import { SlackIcon } from '@/components/icons/SlackIcon'

/** Decorative provider identity; the adjacent text supplies the accessible name. */
export function NotificationProviderIcon({
  provider,
  className = 'size-5',
}: {
  provider: string
  className?: string
}) {
  const Icon =
    provider === 'slack'
      ? SlackIcon
      : provider === 'cloudflare'
        ? CloudflareIcon
        : provider === 'email'
          ? Mail
          : Webhook
  return (
    <span
      aria-hidden="true"
      className="inline-flex shrink-0 text-muted-foreground"
    >
      <Icon className={className} />
    </span>
  )
}
