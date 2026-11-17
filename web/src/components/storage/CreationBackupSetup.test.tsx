// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { CreationBackupSetup } from './CreationBackupSetup'

afterEach(cleanup)
test('a stale destination can be cleared when the list becomes empty or unavailable', () => {
  for (const isError of [false, true]) {
    let chosen = '7'
    const view = render(
      <MemoryRouter>
        <CreationBackupSetup
          query={{ isPending: false, isError, data: [], refetch: () => {} }}
          destination={chosen}
          onDestinationChange={(value) => {
            chosen = value
          }}
        />
      </MemoryRouter>
    )
    fireEvent.click(view.getByRole('button', { name: 'Set up backups later' }))
    expect(chosen).toBe('later')
    if (!isError)
      expect(
        view.getByRole('link', { name: 'Create destination' })
      ).toBeTruthy()
    view.unmount()
  }
})

test('cached destinations stay visible when their background refresh fails', () => {
  const view = render(
    <MemoryRouter>
      <CreationBackupSetup
        query={{
          isPending: false,
          isError: true,
          data: [{ id: 7, name: 'Sample destination' }],
          refetch: () => {},
        }}
        destination="7"
        onDestinationChange={() => {}}
      />
    </MemoryRouter>
  )
  expect(view.getByText('Backup destinations unavailable')).toBeTruthy()
  expect(view.getByRole('combobox')).toBeTruthy()
  expect(view.getByText('Sample destination')).toBeTruthy()
})
