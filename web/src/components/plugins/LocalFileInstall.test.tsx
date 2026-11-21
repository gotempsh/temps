// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'
import { mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { LocalFileInstall } from './LocalFileInstall'

installBrowserWindow()
afterEach(cleanup)

test('binds trust and the upload to the digest shown for the selected file', async () => {
  const host = 'https://console.example.test/api'
  const uploads: { url: string; digest: string | null }[] = []
  const api = mockApi({
    'POST /x/plugins/install/file': (request) => {
      uploads.push({
        url: request.url,
        digest: request.headers.get('X-Temps-Trust-Sha256'),
      })
      return Response.json({
        name: 'fixture',
        version: '1',
        platform: 'test',
        sha256: uploads[0].digest,
      })
    },
  })
  try {
    client.setConfig({ baseUrl: host })
    const view = render(
      <QueryClientProvider client={new QueryClient()}>
        <LocalFileInstall
          open
          expectedName="fixture"
          onOpenChange={() => {}}
          onSensitiveError={() => false}
        />
      </QueryClientProvider>
    )
    const trust = view.getByRole('checkbox') as HTMLButtonElement
    const upload = view.getByLabelText('Executable')
    const choose = (file: File) =>
      fireEvent.change(upload, { target: { files: [file] } })
    expect(trust.disabled).toBe(true)
    expect(view.getByText(host)).toBeTruthy()
    choose(new File(['hello'], 'first'))
    expect(
      await view.findByText(
        '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824'
      )
    ).toBeTruthy()
    fireEvent.click(trust)
    expect(trust.getAttribute('aria-checked')).toBe('true')
    choose(new File(['abc'], 'replacement'))
    expect(trust.getAttribute('aria-checked')).toBe('false')
    const expected =
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'
    expect(await view.findByText(expected)).toBeTruthy()
    fireEvent.change(view.getByLabelText('Manifest version'), {
      target: { value: '1' },
    })
    fireEvent.click(trust)
    fireEvent.click(view.getByRole('button', { name: 'Install plugin' }))
    await waitFor(() =>
      expect(uploads).toEqual([
        {
          url: host + '/x/plugins/install/file?name=fixture&version=1',
          digest: expected,
        },
      ])
    )
  } finally {
    api.restore()
  }
})
