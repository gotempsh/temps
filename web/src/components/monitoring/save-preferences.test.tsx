// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, render } from '@testing-library/react'
import { Toaster, toast } from 'sonner'
import type { NotificationPreferencesResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { savePreferences } from './save-preferences'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const MESSAGES = {
  loading: 'Saving domain alert settings...',
  success: 'Domain alert settings saved successfully',
  subject: 'domain alert settings',
}

const BODY = {
  preferences: {
    ssl_expiration_enabled: true,
  } as NotificationPreferencesResponse,
}

test('a refused save reports the reason instead of success', async () => {
  const api = mockApi({
    'PUT /notification-preferences': () =>
      jsonResponse(
        {
          title: 'Bad Request',
          status: 400,
          detail: 'ssl_days_before_expiration must be between 1 and 90',
        },
        400
      ),
  })
  const view = render(<Toaster />)
  let saved: boolean | undefined
  await act(async () => {
    saved = await savePreferences(BODY, MESSAGES)
  })

  expect(saved).toBe(false)
  expect(
    await view.findByText(
      'Failed to save domain alert settings: ssl_days_before_expiration must be between 1 and 90'
    )
  ).toBeTruthy()
  expect(view.queryByText(MESSAGES.success)).toBeNull()
  api.restore()
})

test('an accepted save reports success', async () => {
  const api = mockApi({
    'PUT /notification-preferences': () => jsonResponse(BODY.preferences),
  })
  const view = render(<Toaster />)
  let saved: boolean | undefined
  await act(async () => {
    saved = await savePreferences(BODY, MESSAGES)
  })

  expect(saved).toBe(true)
  expect(await view.findByText(MESSAGES.success)).toBeTruthy()
  api.restore()
})
