// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { useState } from 'react'
import type { RateLimitSettings } from '@/api/platformSettings'
import { installBrowserWindow } from '@/test/dom'
import { RateLimitFields } from './RateLimitFields'
import { DEFAULT_RATE_LIMITING } from './rate-limiting'

installBrowserWindow()
afterEach(cleanup)

const last = <T,>(items: T[]): T | undefined => items[items.length - 1]

function Harness({
  initial,
  changes,
}: {
  initial: RateLimitSettings | undefined
  changes: RateLimitSettings[]
}) {
  const [value, setValue] = useState(initial)
  return (
    <RateLimitFields
      value={value}
      onChange={(next) => {
        changes.push(next)
        setValue(next)
      }}
    />
  )
}

test('without saved settings it starts from the defaults, switched off', () => {
  const view = render(<Harness initial={undefined} changes={[]} />)
  expect(
    view
      .getByRole('switch', { name: 'Enable rate limiting' })
      .getAttribute('aria-checked')
  ).toBe('false')
  expect(view.queryByLabelText('Max requests per minute')).toBeNull()
})

test('switching it on reveals the limits and keeps the other fields', () => {
  const changes: RateLimitSettings[] = []
  const view = render(
    <Harness
      initial={{ ...DEFAULT_RATE_LIMITING, max_requests_per_hour: 500 }}
      changes={changes}
    />
  )
  fireEvent.click(view.getByRole('switch', { name: 'Enable rate limiting' }))

  expect(last(changes)).toEqual({
    ...DEFAULT_RATE_LIMITING,
    enabled: true,
    max_requests_per_hour: 500,
  })
  const perHour = view.getByLabelText(
    'Max requests per hour'
  ) as HTMLInputElement
  expect(perHour.value).toBe('500')
})

test('a cleared limit shows empty, not NaN', () => {
  const changes: RateLimitSettings[] = []
  const view = render(
    <Harness
      initial={{ ...DEFAULT_RATE_LIMITING, enabled: true }}
      changes={changes}
    />
  )
  const perMinute = view.getByLabelText(
    'Max requests per minute'
  ) as HTMLInputElement
  fireEvent.change(perMinute, { target: { value: '' } })

  expect(Number.isNaN(last(changes)?.max_requests_per_minute)).toBe(true)
  expect(perMinute.value).toBe('')
})

test('whitelist entries can be added, edited and removed', () => {
  const changes: RateLimitSettings[] = []
  const view = render(
    <Harness
      initial={{ ...DEFAULT_RATE_LIMITING, enabled: true }}
      changes={changes}
    />
  )
  fireEvent.click(view.getByRole('button', { name: 'Add whitelist IP' }))
  fireEvent.change(view.getByLabelText('Whitelisted IP 1'), {
    target: { value: '10.0.0.0/24' },
  })
  expect(last(changes)?.whitelist_ips).toEqual(['10.0.0.0/24'])

  fireEvent.click(view.getByRole('button', { name: 'Remove IP address' }))
  expect(last(changes)?.whitelist_ips).toEqual([])
  expect(view.queryByLabelText('Whitelisted IP 1')).toBeNull()
})
