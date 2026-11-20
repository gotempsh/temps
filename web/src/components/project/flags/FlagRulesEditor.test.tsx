// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { EnvironmentResponse, FlagResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { FlagRulesEditor } from './FlagRulesEditor'

installBrowserWindow()
afterEach(cleanup)

const environment = {
  id: 7,
  name: 'production',
  slug: 'production',
} as unknown as EnvironmentResponse

function flagWith(description: string, updatedAt: string): FlagResponse {
  return {
    id: 1,
    key: 'checkout-redesign',
    value_type: 'bool',
    default_value: false,
    client_visible: true,
    exposure_sample_rate: 0,
    variants: [],
    created_at: '2026-11-20T00:00:00Z',
    updated_at: updatedAt,
    environments: [
      {
        environment_id: 7,
        enabled: true,
        value: false,
        rules_valid: true,
        updated_at: updatedAt,
        rules: [
          {
            description,
            conditions: [],
            serve: { kind: 'value', value: true },
          },
        ],
      },
    ],
  }
}

function tree(flag: FlagResponse, client: QueryClient) {
  return (
    <QueryClientProvider client={client}>
      <FlagRulesEditor
        projectId={1}
        flag={flag}
        environment={environment}
        onChanged={() => {}}
      />
    </QueryClientProvider>
  )
}

test('an editor closed before a refetch reopens on the fetched rules', () => {
  const client = new QueryClient()
  const view = render(
    tree(flagWith('Old rule', '2026-11-20T10:00:00Z'), client)
  )
  // Open, then close before the newer rules arrive, as "Discard and
  // reload" does after a conflict.
  fireEvent.click(screen.getByRole('button', { name: 'Edit rules' }))
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  view.rerender(tree(flagWith('Their rule', '2026-11-20T10:05:00Z'), client))

  fireEvent.click(screen.getByRole('button', { name: 'Edit rules' }))
  const description = screen.getByLabelText('Description') as HTMLInputElement
  expect(description.value).toBe('Their rule')
})
