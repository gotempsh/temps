// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, useLocation } from 'react-router'
import type { ProviderPreset } from '@/api/client'
import { CredentialProviderCatalog } from './CredentialProviderCatalog'

import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'

installBrowserWindow()
afterEach(cleanup)

function preset(id: string, name: string): ProviderPreset {
  return {
    id,
    name,
    description: `${name} API`,
    automatic: false,
    documentation_url: `https://example.test/${id}`,
    spec: { url: `https://api.example.test/${id}` },
  } as unknown as ProviderPreset
}

const providers = [
  preset('alpha', 'Alpha'),
  preset('bravo', 'Bravo'),
  preset('charlie', 'Charlie'),
]

function Harness() {
  const location = useLocation()
  return (
    <div>
      <CredentialProviderCatalog
        providers={providers}
        selectedId=""
        onSelect={() => {}}
      />
      <output data-testid="search">{location.search}</output>
    </div>
  )
}

test('provider search keeps typed text, filters, and survives a remount from the URL', async () => {
  const first = render(
    <MemoryRouter initialEntries={['/checks']}>
      <Harness />
    </MemoryRouter>
  )
  const input = first.getByLabelText(
    'Search credential providers'
  ) as HTMLInputElement
  let typed = ''
  for (const char of 'bravo') {
    typed += char
    fireEvent.change(input, { target: { value: typed } })
  }
  expect(input.value).toBe('bravo')
  expect(first.queryByText('Alpha')).toBeNull()
  await waitFor(() => {
    const params = new URLSearchParams(
      first.getByTestId('search').textContent ?? ''
    )
    expect(params.get('provider_q')).toBe('bravo')
  })
  first.unmount()

  const reloaded = render(
    <MemoryRouter initialEntries={['/checks?provider_q=charlie']}>
      <Harness />
    </MemoryRouter>
  )
  expect(
    (reloaded.getByLabelText('Search credential providers') as HTMLInputElement)
      .value
  ).toBe('charlie')
  expect(reloaded.queryByText('Alpha')).toBeNull()
  reloaded.unmount()
})
