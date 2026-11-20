// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ReadFailure } from '@/components/ui/read-failure'

/**
 * What the server answers a role without `settings:read` when it asks for
 * node metrics. The pages that show them render this once instead of sending
 * (and re-sending, on a timer) requests that are certain to be refused.
 */
const NODE_METRICS_REFUSAL = {
  status: 403,
  title: 'Forbidden',
  detail:
    'Reading node metrics requires the settings:read permission, which your role does not include.',
}

export function NodeMetricsAccessDenied() {
  return <ReadFailure resource="Node metrics" error={NODE_METRICS_REFUSAL} />
}
