// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { FunnelResponse } from '@/api/client/types.gen'
import type { FunnelFormData } from './FunnelForm'

/** The edit form's starting values: the funnel as saved, steps in order. */
export function funnelFormData(funnel: FunnelResponse): FunnelFormData {
  return {
    name: funnel.name,
    description: funnel.description || '',
    steps: [...funnel.steps]
      .sort((a, b) => a.order - b.order)
      .map((step) => ({
        event_name: step.event_name,
        event_filter: step.event_filter,
        showFilters: step.event_filter.length > 0,
      })),
  }
}
