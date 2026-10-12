// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SandboxFilesCard } from './SandboxFilesCard'
import { encodeText } from './sandbox-files'

installBrowserWindow()
afterEach(cleanup)

function renderCard(props: { status?: string; lifecycle?: string } = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <SandboxFilesCard
        sandboxId="sbx_1"
        workDir="/home/user/work"
        status={props.status ?? 'running'}
        lifecycle={props.lifecycle}
      />
    </QueryClientProvider>
  )
}

const PATH = '/home/user/work/notes.txt'

test('opens a file, edits it and saves the new contents as base64', async () => {
  let written: unknown
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 6,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () =>
      jsonResponse({
        path: PATH,
        size: 6,
        contents_b64: encodeText('hello\n'),
      }),
    'POST /v1/sandboxes/sbx_1/fs/write': async (request) => {
      written = await request.json()
      return new Response(null, { status: 204 })
    },
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: PATH },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))

  const editor = (await view.findByLabelText(
    `Contents of ${PATH}`
  )) as HTMLTextAreaElement
  expect(editor.value).toBe('hello\n')
  const save = view.getByRole('button', { name: 'Save' }) as HTMLButtonElement
  expect(save.disabled).toBe(true)

  fireEvent.change(editor, { target: { value: 'héllo, world\n' } })
  expect(save.disabled).toBe(false)
  fireEvent.click(save)

  await view.findByText(`Saved ${PATH}`).catch(() => null)
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(written).toEqual({
    path: PATH,
    contents_b64: encodeText('héllo, world\n'),
  })
  api.restore()
})

test('a missing file opens as a new file that Save will create', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: false,
        is_file: false,
        is_dir: false,
        size: 0,
      }),
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: PATH },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))

  await view.findByText('new file')
  expect(api.called('GET', '/v1/sandboxes/sbx_1/fs/read')).toBe(false)
  api.restore()
})

test('a directory is explained instead of being read', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: '/home/user/work/src',
        exists: true,
        is_file: false,
        is_dir: true,
        size: 4096,
      }),
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: '/home/user/work/src' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))

  await view.findByText(/is a directory/)
  expect(api.called('GET', '/v1/sandboxes/sbx_1/fs/read')).toBe(false)
  api.restore()
})

test('a relative path is rejected before any request', async () => {
  const api = mockApi({})
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: 'notes.txt' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))

  await view.findByText('The path must be absolute, starting with /.')
  expect(api.called('GET', '/v1/sandboxes/sbx_1/fs/stat')).toBe(false)
  api.restore()
})

test('a stopped ephemeral sandbox explains itself and blocks the form', () => {
  const api = mockApi({})
  const view = renderCard({ status: 'stopped' })

  expect(view.getByText(/Resume it to read or write files/)).toBeTruthy()
  expect(
    (view.getByRole('button', { name: 'Open file' }) as HTMLButtonElement)
      .disabled
  ).toBe(true)
  api.restore()
})

test('opening the same file again reloads it, so a stale buffer is never saved over a newer file', async () => {
  let current = 'from the terminal, version 1\n'
  let reads = 0
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: current.length,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () => {
      reads += 1
      return jsonResponse({
        path: PATH,
        size: current.length,
        contents_b64: encodeText(current),
      })
    },
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: PATH },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  const first = (await view.findByLabelText(
    `Contents of ${PATH}`
  )) as HTMLTextAreaElement
  expect(first.value).toBe('from the terminal, version 1\n')

  // Something else (the terminal card beside this one) changes the file.
  current = 'from the terminal, version 2\n'
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))

  await waitFor(() => {
    const editor = view.getByLabelText(
      `Contents of ${PATH}`
    ) as HTMLTextAreaElement
    expect(editor.value).toBe('from the terminal, version 2\n')
  })
  expect(reads).toBe(2)
  api.restore()
})
