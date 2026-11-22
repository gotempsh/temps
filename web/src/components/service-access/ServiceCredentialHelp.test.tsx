// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, within } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { ServiceCredentialHelp } from './ServiceCredentialHelp'

installBrowserWindow()
afterEach(cleanup)

test('a known service links to the page that creates its credential', () => {
  render(
    <ServiceCredentialHelp
      entry={{
        name: 'Notion',
        credential_help: {
          url: 'https://www.notion.so/my-integrations',
          notes: ['Integration secret: Share the pages with the integration.'],
        },
      }}
    />
  )
  const link = within(document.body).getByRole('link', {
    name: /Open Notion credentials/,
  })
  expect(link.getAttribute('href')).toBe(
    'https://www.notion.so/my-integrations'
  )
  expect(link.getAttribute('target')).toBe('_blank')
  expect(link.getAttribute('rel')).toContain('noopener')
  expect(document.body.textContent).toContain('Share the pages')
})

test('a service without a known page says where to look and invents no link', () => {
  render(
    <ServiceCredentialHelp
      entry={{ name: 'Example', credential_help: { notes: [] } }}
    />
  )
  expect(within(document.body).queryByRole('link')).toBeNull()
  expect(document.body.textContent).toContain(
    "Create an API key or access token in your Example account's developer or API settings"
  )
})
