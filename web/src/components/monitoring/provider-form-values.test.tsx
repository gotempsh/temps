// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, renderHook } from '@testing-library/react'
import { useForm } from 'react-hook-form'
import type { NotificationProviderResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import {
  providerFormValues,
  useLoadProviderIntoForm,
} from './provider-form-values'
import type { ProviderFormData } from './schemas'

installBrowserWindow()
afterEach(cleanup)

function telegramProvider(
  id: number,
  chatId: string | number,
  name = 'Phone alerts'
): NotificationProviderResponse {
  return {
    id,
    name,
    provider_type: 'telegram',
    enabled: true,
    created_at: '1970-01-01T00:00:00Z',
    updated_at: '1970-01-01T00:00:00Z',
    config: { bot_token: '***', chat_id: chatId, message_thread_id: null },
  }
}

function renderEditForm(initial: NotificationProviderResponse | undefined) {
  return renderHook(
    ({ provider }: { provider: NotificationProviderResponse | undefined }) => {
      const form = useForm<ProviderFormData>()
      useLoadProviderIntoForm(provider, form)
      return form
    },
    { initialProps: { provider: initial } }
  )
}

test('loads the provider once it arrives', () => {
  const { result, rerender } = renderEditForm(undefined)
  expect(result.current.getValues('name')).toBeUndefined()

  rerender({ provider: telegramProvider(1, '-1001') })
  expect(result.current.getValues('name')).toBe('Phone alerts')
  expect(result.current.getValues('config.chat_id')).toBe('-1001')
})

test('a refetch of the same provider keeps unsaved edits', () => {
  const { result, rerender } = renderEditForm(telegramProvider(1, '-1001'))

  act(() => {
    result.current.setValue('config.bot_token', '123456789:AAReplacement', {
      shouldDirty: true,
    })
    result.current.setValue('name', 'Renamed', { shouldDirty: true })
  })

  // Another administrator changed the chat; a reconnect refetch brings it in.
  rerender({ provider: telegramProvider(1, '-1002', 'Changed elsewhere') })

  expect(result.current.getValues('config.bot_token')).toBe(
    '123456789:AAReplacement'
  )
  expect(result.current.getValues('name')).toBe('Renamed')
  // The field the user did not touch takes the newer value.
  expect(result.current.getValues('config.chat_id')).toBe('-1002')
})

test('reopening with cached data loads the newer response that follows', () => {
  // The cache still holds the provider as it was before the last save.
  const { result, rerender } = renderEditForm(telegramProvider(1, '-1001'))
  expect(result.current.getValues('config.chat_id')).toBe('-1001')

  // The background refetch returns the saved chat.
  rerender({ provider: telegramProvider(1, '-1002', 'Phone alerts') })

  expect(result.current.getValues('config.chat_id')).toBe('-1002')
  expect(result.current.formState.defaultValues?.config?.chat_id).toBe('-1002')
})

test('switching to another provider loads that provider', () => {
  const { result, rerender } = renderEditForm(telegramProvider(1, '-1001'))
  act(() => {
    result.current.setValue('name', 'Draft for provider 1', {
      shouldDirty: true,
    })
  })

  rerender({ provider: telegramProvider(2, '@ops_channel', 'Other provider') })

  expect(result.current.getValues('name')).toBe('Other provider')
  expect(result.current.getValues('config.chat_id')).toBe('@ops_channel')
})

test('an integer chat id from the generic API loads as a string', () => {
  const values = providerFormValues(telegramProvider(1, -1001234567890))
  expect(values.config.chat_id).toBe('-1001234567890')
})
