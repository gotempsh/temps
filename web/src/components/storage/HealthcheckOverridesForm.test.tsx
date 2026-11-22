// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import type {
  HealthcheckOverrides,
  HealthcheckTimings,
} from '@/api/client/types.gen'
import type { InheritedHealthcheck } from '@/lib/service-healthcheck'
import { installBrowserWindow } from '@/test/dom'
import { HealthcheckOverridesForm } from './HealthcheckOverridesForm'

installBrowserWindow()
afterEach(cleanup)

const REDIS: HealthcheckTimings = {
  interval_secs: 30,
  start_interval_secs: 2,
  start_period_secs: 90,
  timeout_secs: 5,
  retries: 3,
}
const INHERITED: InheritedHealthcheck[] = [
  { timings: REDIS, readinessWaitSecs: 90 },
]
const NONE: HealthcheckOverrides = {
  interval_secs: null,
  start_interval_secs: null,
  start_period_secs: null,
  timeout_secs: null,
  retries: null,
}

function form(
  overrides: HealthcheckOverrides,
  onSave: (o: HealthcheckOverrides) => void = () => {}
) {
  return (
    <HealthcheckOverridesForm
      overrides={overrides}
      inherited={INHERITED}
      inheritedFrom="the instance default"
      saving={false}
      onSave={onSave}
    />
  )
}

const input = (view: ReturnType<typeof render>, label: string) =>
  view.getByLabelText(label) as HTMLInputElement

test("a refetch of someone else's save keeps the field being edited", async () => {
  const view = render(form({ ...NONE, interval_secs: 60 }))
  fireEvent.change(input(view, 'Interval (s)'), { target: { value: '120' } })
  await waitFor(() => expect(view.getByText('Discard changes')).toBeTruthy())

  // Another operator saved retries meanwhile; the card refetches.
  view.rerender(form({ ...NONE, interval_secs: 60, retries: 5 }))

  await waitFor(() => expect(input(view, 'Retries').value).toBe('5'))
  expect(input(view, 'Interval (s)').value).toBe('120')
  expect(view.getByText('Discard changes')).toBeTruthy()
})

test('its own save coming back leaves the form clean', async () => {
  const onSave = mock((_: HealthcheckOverrides) => {})
  const view = render(form({ ...NONE, interval_secs: 60 }, onSave))
  fireEvent.change(input(view, 'Interval (s)'), { target: { value: '120' } })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save' }))
  })
  await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1))
  const sent = onSave.mock.calls[0][0]
  expect(sent.interval_secs).toBe(120)

  view.rerender(form(sent, onSave))

  await waitFor(() => expect(view.queryByText('Discard changes')).toBeNull())
  expect(input(view, 'Interval (s)').value).toBe('120')
})
