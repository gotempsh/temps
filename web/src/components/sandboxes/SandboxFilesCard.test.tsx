// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SandboxFilesCard } from './SandboxFilesCard'
import { encodeText, filePropertiesArgv } from './sandbox-files'

installBrowserWindow()
afterEach(cleanup)

interface CardProps {
  status?: string
  lifecycle?: string
  backend?: string | null
}

function cardTree(client: QueryClient, props: CardProps) {
  return (
    <QueryClientProvider client={client}>
      <SandboxFilesCard
        sandboxId="sbx_1"
        workDir="/home/temps/workspace"
        status={props.status ?? 'running'}
        lifecycle={props.lifecycle}
        // Docker unless a test says otherwise; the save warnings depend on it.
        backend={props.backend === undefined ? 'docker' : props.backend}
      />
    </QueryClientProvider>
  )
}

function newClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
}

function renderCard(props: CardProps = {}) {
  return render(cardTree(newClient(), props))
}

const PATH = '/home/temps/workspace/notes.txt'

/** `stat -c %a` answer for the file's permission bits. */
const modeRoute = (stdout = '644 1000\n', exitCode = 0) => ({
  'POST /v1/sandboxes/sbx_1/exec': () =>
    jsonResponse({ exit_code: exitCode, stdout, stderr: '' }),
})

test('opens a file, edits it and saves the new contents as base64', async () => {
  let written: unknown
  const api = mockApi({
    ...modeRoute(),
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
    mode: 0o644,
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
        path: '/home/temps/workspace/src',
        exists: true,
        is_file: false,
        is_dir: true,
        size: 4096,
      }),
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: '/home/temps/workspace/src' },
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
    ...modeRoute(),
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

test('opening a missing path again keeps the new-file editor and never reads it', async () => {
  let reads = 0
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: false,
        is_file: false,
        is_dir: false,
        size: 0,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () => {
      reads += 1
      return jsonResponse({ title: 'Not Found' }, 404)
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
  fireEvent.change(editor, { target: { value: 'draft' } })

  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  await new Promise((resolve) => setTimeout(resolve, 100))

  expect(reads).toBe(0)
  expect(view.queryByText(/Could not read/)).toBeNull()
  expect(view.getByLabelText(`Contents of ${PATH}`)).toBeTruthy()
  api.restore()
})

test('opening an oversized file again does not fetch its contents', async () => {
  let reads = 0
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 10 * 1024 * 1024,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () => {
      reads += 1
      return jsonResponse({ path: PATH, size: 0, contents_b64: '' })
    },
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: PATH },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  await view.findByText(/over the/)
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  await new Promise((resolve) => setTimeout(resolve, 100))

  expect(reads).toBe(0)
  api.restore()
})

test('a file created since the first open is loaded when the path is opened again', async () => {
  let exists = false
  let reads = 0
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists,
        is_file: exists,
        is_dir: false,
        size: exists ? 5 : 0,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () => {
      reads += 1
      return jsonResponse({
        path: PATH,
        size: 5,
        contents_b64: encodeText('made!'),
      })
    },
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: PATH },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  await view.findByText('new file')

  exists = true
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))

  await waitFor(() => {
    const editor = view.getByLabelText(
      `Contents of ${PATH}`
    ) as HTMLTextAreaElement
    expect(editor.value).toBe('made!')
  })
  expect(reads).toBe(1)
  api.restore()
})

test('text typed while a save is in flight is kept and still marked unsaved', async () => {
  let release: () => void = () => {}
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  const written: string[] = []
  const api = mockApi({
    ...modeRoute(),
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 5,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () =>
      jsonResponse({ path: PATH, size: 5, contents_b64: encodeText('start') }),
    'POST /v1/sandboxes/sbx_1/fs/write': async (request) => {
      const body = (await request.json()) as { contents_b64: string }
      written.push(body.contents_b64)
      await gate
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

  fireEvent.change(editor, { target: { value: 'first' } })
  fireEvent.click(view.getByRole('button', { name: 'Save' }))
  await waitFor(() => expect(written).toHaveLength(1))

  // The request is still pending; the operator keeps typing.
  fireEvent.change(editor, { target: { value: 'first, and more' } })
  const saveButton = view.getByRole('button', {
    name: 'Save',
  }) as HTMLButtonElement
  expect(saveButton.disabled).toBe(true)
  release()

  // Pending ends once the write resolved; then let the cache settle.
  await waitFor(() => expect(saveButton.disabled).toBe(false))
  await new Promise((resolve) => setTimeout(resolve, 100))

  const after = view.getByLabelText(
    `Contents of ${PATH}`
  ) as HTMLTextAreaElement
  expect(after.value).toBe('first, and more')
  expect(view.getByText('unsaved changes')).toBeTruthy()
  expect(written).toEqual([encodeText('first')])
  api.restore()
})

test('a sandbox that stops keeps the unsaved edit, read-only, and resumes with it intact', async () => {
  const api = mockApi({
    ...modeRoute(),
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 5,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () =>
      jsonResponse({ path: PATH, size: 5, contents_b64: encodeText('saved') }),
  })
  const client = newClient()
  const view = render(cardTree(client, { status: 'running' }))

  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: PATH },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  const editor = (await view.findByLabelText(
    `Contents of ${PATH}`
  )) as HTMLTextAreaElement
  fireEvent.change(editor, { target: { value: 'unsaved work' } })

  // The sandbox stops (ephemeral): file actions are blocked, the text is not.
  view.rerender(cardTree(client, { status: 'stopped' }))
  const stopped = view.getByLabelText(
    `Contents of ${PATH}`
  ) as HTMLTextAreaElement
  expect(stopped).toBe(editor)
  expect(stopped.value).toBe('unsaved work')
  expect(stopped.readOnly).toBe(true)
  expect(
    (view.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled
  ).toBe(true)
  expect(view.getByText(/Resume it to read or write files/)).toBeTruthy()

  view.rerender(cardTree(client, { status: 'running' }))
  const resumed = view.getByLabelText(
    `Contents of ${PATH}`
  ) as HTMLTextAreaElement
  expect(resumed.value).toBe('unsaved work')
  expect(resumed.readOnly).toBe(false)
  expect(view.getByText('unsaved changes')).toBeTruthy()
  api.restore()
})

test("saving sends the file's own mode so an executable keeps its bit", async () => {
  let body: { mode?: number } | undefined
  const execCalls: string[][] = []
  const api = mockApi({
    'POST /v1/sandboxes/sbx_1/exec': async (request) => {
      const payload = (await request.json()) as { cmd: string[] }
      execCalls.push(payload.cmd)
      return jsonResponse({ exit_code: 0, stdout: '755 1000\n', stderr: '' })
    },
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 12,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () =>
      jsonResponse({
        path: PATH,
        size: 12,
        contents_b64: encodeText('#!/bin/sh\n'),
      }),
    'POST /v1/sandboxes/sbx_1/fs/write': async (request) => {
      body = (await request.json()) as { mode?: number }
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
  expect(view.getByText('0755')).toBeTruthy()

  fireEvent.change(editor, { target: { value: '#!/bin/sh\necho hi\n' } })
  fireEvent.click(view.getByRole('button', { name: 'Save' }))

  await waitFor(() => expect(body).toBeDefined())
  expect(body?.mode).toBe(0o755)
  // One probe when the file is opened and one when Save is pressed, so the
  // decision is made from the file as it is then.
  expect(execCalls).toEqual([
    filePropertiesArgv(PATH),
    filePropertiesArgv(PATH),
  ])
  api.restore()
})

test('an unreadable mode asks before saving, and does nothing if declined', async () => {
  const writes: unknown[] = []
  const api = mockApi({
    ...modeRoute('', 1),
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 5,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () =>
      jsonResponse({ path: PATH, size: 5, contents_b64: encodeText('start') }),
    'POST /v1/sandboxes/sbx_1/fs/write': async (request) => {
      writes.push(await request.json())
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
  fireEvent.change(editor, { target: { value: 'changed' } })
  fireEvent.click(view.getByRole('button', { name: 'Save' }))

  await view.findByText(/could not be read, so it will be reset to 0644/)
  expect(writes).toHaveLength(0)

  fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(writes).toHaveLength(0)

  fireEvent.click(view.getByRole('button', { name: 'Save' }))
  fireEvent.click(await view.findByRole('button', { name: 'Save anyway' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  expect((writes[0] as { mode: number }).mode).toBe(0o644)
  api.restore()
})

test('a new file is written at 0644 without reading any mode', async () => {
  let execCalled = false
  let body: { mode?: number } | undefined
  const api = mockApi({
    'POST /v1/sandboxes/sbx_1/exec': () => {
      execCalled = true
      return jsonResponse({ exit_code: 0, stdout: '644 1000\n', stderr: '' })
    },
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path: PATH,
        exists: false,
        is_file: false,
        is_dir: false,
        size: 0,
      }),
    'POST /v1/sandboxes/sbx_1/fs/write': async (request) => {
      body = (await request.json()) as { mode?: number }
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
  fireEvent.change(editor, { target: { value: 'hello' } })
  fireEvent.click(view.getByRole('button', { name: 'Save' }))

  await waitFor(() => expect(body).toBeDefined())
  expect(body?.mode).toBe(0o644)
  expect(execCalled).toBe(false)
  api.restore()
})

async function openAndSave(
  view: ReturnType<typeof renderCard>,
  path: string,
  text: string
) {
  fireEvent.change(view.getByLabelText('File path'), {
    target: { value: path },
  })
  fireEvent.click(view.getByRole('button', { name: 'Open file' }))
  const editor = (await view.findByLabelText(
    `Contents of ${path}`
  )) as HTMLTextAreaElement
  fireEvent.change(editor, { target: { value: text } })
  fireEvent.click(view.getByRole('button', { name: 'Save' }))
}

function fileRoutes(
  path: string,
  props: string,
  writes: Array<{ mode: number }>
) {
  return {
    ...modeRoute(props),
    'GET /v1/sandboxes/sbx_1/fs/stat': () =>
      jsonResponse({
        path,
        exists: true,
        is_file: true,
        is_dir: false,
        size: 3,
      }),
    'GET /v1/sandboxes/sbx_1/fs/read': () =>
      jsonResponse({ path, size: 3, contents_b64: encodeText('old') }),
    'POST /v1/sandboxes/sbx_1/fs/write': async (request: Request) => {
      writes.push((await request.json()) as { mode: number })
      return new Response(null, { status: 204 })
    },
  }
}

test('a symlinked file is probed through the link and warned about before it is replaced', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '644 1000\nsymlink\n', writes))
  const view = renderCard()

  await openAndSave(view, PATH, 'new')

  await view.findByText(/replaces the link with a regular file/)
  expect(writes).toHaveLength(0)
  fireEvent.click(await view.findByRole('button', { name: 'Save anyway' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  // The target's 0644, not the link's own 0777.
  expect(writes[0]!.mode).toBe(0o644)
  api.restore()
})

test('a sandbox-user file outside the home is warned about, with its mode kept', async () => {
  const writes: Array<{ mode: number }> = []
  const outside = '/srv/app/.env'
  const api = mockApi(fileRoutes(outside, '600 1000\n', writes))
  const view = renderCard()

  await openAndSave(view, outside, 'SECRET=2')

  await view.findByText(/the sandbox writes files as root/)
  expect(writes).toHaveLength(0)
  fireEvent.click(await view.findByRole('button', { name: 'Save anyway' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  expect(writes[0]!.mode).toBe(0o600)
  api.restore()
})

test('setuid is warned about and not written back', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '4755 1000\n', writes))
  const view = renderCard()

  await openAndSave(view, PATH, 'new')

  await view.findByText(/setuid, setgid or sticky bit is not kept/)
  fireEvent.click(await view.findByRole('button', { name: 'Save anyway' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  expect(writes[0]!.mode).toBe(0o755)
  api.restore()
})

test('a root-owned file inside the home is warned about, because a save gives it to the sandbox user', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '644 0\n', writes))
  const view = renderCard()

  await openAndSave(view, PATH, 'new')

  await view.findByText(/currently owned by uid 0/)
  expect(writes).toHaveLength(0)
  fireEvent.click(await view.findByRole('button', { name: 'Save anyway' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  expect(writes[0]!.mode).toBe(0o644)
  api.restore()
})

test('a file replaced by a symlink after it was opened is caught when Save is pressed', async () => {
  const writes: Array<{ mode: number }> = []
  // The probe at open time sees a regular file; the one at save time sees the
  // link that the terminal put there in between.
  const probes = ['644 1000\n', '644 1000\nsymlink\n']
  let probeCount = 0
  const api = mockApi({
    ...fileRoutes(PATH, '644 1000\n', writes),
    'POST /v1/sandboxes/sbx_1/exec': () =>
      jsonResponse({
        exit_code: 0,
        stdout: probes[Math.min(probeCount++, probes.length - 1)],
        stderr: '',
      }),
  })
  const view = renderCard()

  await openAndSave(view, PATH, 'typed by the operator')

  await view.findByText(/replaces the link with a regular file/)
  expect(probeCount).toBe(2)
  expect(writes).toHaveLength(0)
  // The probe never touches what the operator typed.
  expect(
    (view.getByLabelText(`Contents of ${PATH}`) as HTMLTextAreaElement).value
  ).toBe('typed by the operator')

  fireEvent.click(await view.findByRole('button', { name: 'Save anyway' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  api.restore()
})

test('declining the warning writes nothing and keeps the text', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '644 0\n', writes))
  const view = renderCard()

  await openAndSave(view, PATH, 'kept')
  await view.findByText(/currently owned by uid 0/)
  fireEvent.click(view.getByRole('button', { name: 'Cancel' }))

  await waitFor(() =>
    expect(view.queryByText(/currently owned by uid 0/)).toBeNull()
  )
  expect(writes).toHaveLength(0)
  expect(
    (view.getByLabelText(`Contents of ${PATH}`) as HTMLTextAreaElement).value
  ).toBe('kept')
  api.restore()
})

test('on a Firecracker sandbox a symlinked file is written through without a warning', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '644 1000\nsymlink\n', writes))
  const view = renderCard({ backend: 'firecracker' })

  await openAndSave(view, PATH, 'new')

  // No dialog: that backend writes in place, so nothing but the text changes.
  await waitFor(() => expect(writes).toHaveLength(1))
  expect(view.queryByText(/replaces the link/)).toBeNull()
  expect(view.queryByRole('button', { name: 'Save anyway' })).toBeNull()
  expect(writes[0]!.mode).toBe(0o644)
  api.restore()
})

test('on a Firecracker sandbox a root-owned file keeps its owner, so no ownership warning', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '644 0\n', writes))
  const view = renderCard({ backend: 'firecracker' })

  await openAndSave(view, PATH, 'new')

  await waitFor(() => expect(writes).toHaveLength(1))
  expect(view.queryByText(/currently owned by uid 0/)).toBeNull()
  api.restore()
})

test('a backend with undocumented write semantics gets a hedged warning', async () => {
  const writes: Array<{ mode: number }> = []
  const api = mockApi(fileRoutes(PATH, '644 1000\nsymlink\n', writes))
  const view = renderCard({ backend: 'microsandbox' })

  await openAndSave(view, PATH, 'new')

  await view.findByText(/Depending on the sandbox backend/)
  expect(writes).toHaveLength(0)
  api.restore()
})
