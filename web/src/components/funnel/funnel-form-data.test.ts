// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { FunnelResponse } from '@/api/client/types.gen'
import { funnelFormData } from './funnel-form-data'

test('the edit form starts from the funnel as saved, steps in order', () => {
  const funnel: FunnelResponse = {
    id: 4,
    project_id: 2,
    name: 'Signup',
    description: null,
    is_active: true,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    steps: [
      { id: 21, order: 2, event_name: 'signup', event_filter: [] },
      {
        id: 20,
        order: 1,
        event_name: 'page_view',
        event_filter: [{ type: 'page_path', value: '/pricing' }],
      },
    ],
  }
  expect(funnelFormData(funnel)).toEqual({
    name: 'Signup',
    description: '',
    steps: [
      {
        event_name: 'page_view',
        event_filter: [{ type: 'page_path', value: '/pricing' }],
        showFilters: true,
      },
      { event_name: 'signup', event_filter: [], showFilters: false },
    ],
  })
})
