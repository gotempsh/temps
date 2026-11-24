// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { LucideIcon } from 'lucide-react'
import {
  Globe,
  Headphones,
  Link,
  Mail,
  Megaphone,
  MessageSquare,
  Search,
  Share2,
  Smartphone,
  Tag,
  Video,
  Zap,
} from 'lucide-react'

/** An icon for each marketing channel the analytics pipeline assigns. */
export const CHANNEL_ICONS: Record<string, LucideIcon> = {
  Direct: Link,
  Organic: Search,
  'Organic Search': Search,
  'Organic Social': Share2,
  Paid: Megaphone,
  'Paid Search': Megaphone,
  'Paid Social': Megaphone,
  Social: Share2,
  Referral: Globe,
  Email: Mail,
  SMS: MessageSquare,
  Display: Tag,
  Affiliate: Zap,
  Video: Video,
  Audio: Headphones,
  'Mobile App': Smartphone,
}

/** The channel's icon; any channel without its own falls back to a globe. */
export function channelIcon(channel: string | null | undefined): LucideIcon {
  return (channel && CHANNEL_ICONS[channel]) || Globe
}
