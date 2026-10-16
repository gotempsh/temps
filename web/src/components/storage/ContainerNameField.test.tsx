// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  ContainerNameField,
  type ContainerNameSettings,
} from './ContainerNameField'

const settings: ContainerNameSettings = {
  container_name: 'redis-temps-kv-3fa91c07',
  default_container_name: 'redis-temps-kv-3fa91c07',
  custom: false,
  in_use: false,
  required_prefix: 'redis-',
}

function render(
  props: Partial<Parameters<typeof ContainerNameField>[0]> = {}
) {
  return renderToStaticMarkup(
    <ContainerNameField
      serviceName="KV Store"
      settings={settings}
      isLoading={false}
      canManage
      isSaving={false}
      onSave={() => {}}
      onRetry={() => {}}
      {...props}
    />
  )
}

describe('ContainerNameField', () => {
  test('shows the container before the service is enabled and offers to change it', () => {
    const html = render()

    expect(html).toContain('redis-temps-kv-3fa91c07')
    expect(html).toContain('Default name for this instance')
    expect(html).toContain('Change name')
    expect(html).toContain('already uses it')
  })

  test('says when the name is a custom one', () => {
    const html = render({
      settings: { ...settings, custom: true, container_name: 'redis-mine' },
    })

    expect(html).toContain('redis-mine')
    expect(html).toContain('Custom name for this instance')
  })

  test('cannot be changed while the service exists', () => {
    const html = render({ settings: { ...settings, in_use: true } })

    expect(html).toContain('redis-temps-kv-3fa91c07')
    expect(html).toContain('cannot change while the service exists')
    expect(html).not.toContain('Change name')
  })

  test('read-only for users who cannot manage platform services', () => {
    const html = render({ canManage: false })

    expect(html).toContain('redis-temps-kv-3fa91c07')
    expect(html).not.toContain('Change name')
  })

  test('shows a skeleton while loading, never an empty name', () => {
    const html = render({ settings: undefined, isLoading: true })

    expect(html).not.toContain('Docker container')
    expect(html).toContain('animate-pulse')
  })

  test('explains a failed read and offers a retry', () => {
    const html = render({
      settings: undefined,
      error: new Error('boom'),
    })

    expect(html).toContain('could not be read')
    expect(html).toContain('Retry')
    expect(html).not.toContain('Change name')
  })
})
