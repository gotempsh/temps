// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RateLimitSettings } from '@/api/platformSettings'

export const DEFAULT_RATE_LIMITING: RateLimitSettings = {
  enabled: false,
  max_requests_per_minute: 60,
  max_requests_per_hour: 1000,
  whitelist_ips: [],
  blacklist_ips: [],
}

/** What the group says about itself, the same on every page that shows it. */
export const RATE_LIMITING_DESCRIPTION =
  'Limit how many requests each IP address can make'
