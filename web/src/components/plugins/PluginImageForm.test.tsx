// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { toast } from 'sonner'
import { useAddPluginFromImage } from '@/hooks/usePlugins'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  PluginImageFailure,
  PluginImageForm,
  PluginImageSteps,
  type PluginImageChoice,
} from './PluginImageForm'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'success').mockRestore()
})

const IMAGE = 'registry.example.com/acme/reports:1.2.0'

const markup = (node: React.ReactNode) =>
  renderToStaticMarkup(<MemoryRouter>{node}</MemoryRouter>)

test('nothing is shown about steps before the first run', () => {
  expect(
    markup(<PluginImageSteps at={null} running={false} failed={false} />)
  ).toBe('')
})

test('each step says whether it is done, running, failed or still to come', () => {
  const running = markup(<PluginImageSteps at="live" running failed={false} />)
  expect(running).toContain(
    'Create the service</span><span class="sr-only">: Done'
  )
  expect(running).toContain(
    'Deploy the image</span><span class="sr-only">: Done'
  )
  expect(running).toContain(
    'Wait until it is live</span><span class="sr-only">: In progress'
  )
  expect(running).toContain(
    'Add it as a plugin</span><span class="sr-only">: Not started'
  )

  const failed = markup(<PluginImageSteps at="live" running={false} failed />)
  expect(failed).toContain(
    'Wait until it is live</span><span class="sr-only">: Failed'
  )
  expect(failed).toContain(
    'Add it as a plugin</span><span class="sr-only">: Not started'
  )

  // Neither running nor failed: the last step finished as well.
  const finished = markup(
    <PluginImageSteps at="add" running={false} failed={false} />
  )
  expect(finished.match(/: Done/g)).toHaveLength(4)
})

test('a failure gives the reason and links the service that was kept', () => {
  const html = markup(
    <PluginImageFailure
      error={{
        title: 'The deployment did not become live',
        detail: 'It exited.',
      }}
      progress={{ projectSlug: 'reports' }}
    />
  )
  expect(html).toContain('It exited.')
  expect(html).toContain('href="/projects/reports"')
  expect(html).toContain('was created and is kept')
  // Nothing was created: there is nothing to link.
  expect(
    markup(<PluginImageFailure error={{ detail: 'Refused.' }} progress={{}} />)
  ).not.toContain('is kept')
})

function Harness({
  initial,
  onAdded,
  onCancel,
  onSensitiveError = () => false,
}: {
  initial?: PluginImageChoice
  onAdded: (name: string) => void
  onCancel: () => void
  onSensitiveError?: (error: unknown, retry: () => void) => boolean
}) {
  const flow = useAddPluginFromImage()
  return (
    <PluginImageForm
      open
      configured
      initial={initial}
      flow={flow}
      onSensitiveError={onSensitiveError}
      onAdded={onAdded}
      onCancel={onCancel}
    />
  )
}

function renderForm(props: React.ComponentProps<typeof Harness>) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <Harness {...props} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function temps({ status = 'completed', refuseAdd = false } = {}) {
  const sent: Record<string, unknown> = {}
  const api = mockApi({
    'GET /projects/by-slug/reports': () =>
      jsonResponse({ title: 'Not found' }, 404),
    'GET /projects/by-slug/audit': () =>
      jsonResponse({ title: 'Not found' }, 404),
    'POST /projects': async (request) => {
      sent.project = await request.json()
      return jsonResponse({ id: 7, name: 'reports', slug: 'reports' }, 201)
    },
    'GET /projects/7/environments': () =>
      jsonResponse([{ id: 9, name: 'production' }]),
    'POST /projects/7/environments/9/deploy/image': async (request) => {
      sent.deploy = await request.json()
      return jsonResponse({ id: 42, slug: 'deploy-42', project_id: 7 }, 202)
    },
    'GET /projects/7/deployments/42': () => jsonResponse({ id: 42, status }),
    'POST /x/plugins/services': () =>
      refuseAdd
        ? jsonResponse(
            { title: 'Not a plugin', detail: 'The service did not answer.' },
            400
          )
        : jsonResponse(
            {
              name: 'reports',
              version: '1.2.0',
              project_id: 7,
              environment_id: 9,
            },
            201
          ),
  })
  return { api, sent }
}

test('one click creates the service, deploys the image and adds the plugin', async () => {
  const announced = spyOn(toast, 'success').mockImplementation(() => 'id')
  const { api, sent } = temps()
  const added: string[] = []
  let closed = 0
  const view = renderForm({
    onAdded: (name) => added.push(name),
    onCancel: () => closed++,
  })

  fireEvent.change(view.getByLabelText('Image'), { target: { value: IMAGE } })
  // The service is named after the image; nothing else has to be typed.
  expect((view.getByLabelText('Service name') as HTMLInputElement).value).toBe(
    'reports'
  )
  expect((view.getByLabelText('Port') as HTMLInputElement).value).toBe('8080')
  expect(view.getByText(/A tag can be moved to other content/)).toBeTruthy()
  expect(view.getByText(`temps plugin add --image ${IMAGE}`)).toBeTruthy()

  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))
  await waitFor(() => expect(added).toEqual(['reports']))

  expect(sent.project).toMatchObject({ name: 'reports', exposed_port: 8080 })
  expect(sent.deploy).toEqual({
    image_ref: IMAGE,
    health_check_path: '/health',
  })
  expect(api.called('POST', '/x/plugins/services')).toBe(true)
  expect(closed).toBe(1)
  expect(announced).toHaveBeenCalledTimes(1)
  api.restore()
})

test('a listing fills in its image and names the service after the plugin', async () => {
  const { api, sent } = temps()
  const added: string[] = []
  const digest = `registry.example.com/acme/reports-plugin@sha256:${'a'.repeat(64)}`
  const view = renderForm({
    initial: { image: digest, name: 'reports' },
    onAdded: (name) => added.push(name),
    onCancel: () => {},
  })
  expect((view.getByLabelText('Image') as HTMLInputElement).value).toBe(digest)
  expect((view.getByLabelText('Service name') as HTMLInputElement).value).toBe(
    'reports'
  )
  // Named by digest: there is no tag to warn about.
  expect(view.queryByText(/A tag can be moved/)).toBeNull()

  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))
  await waitFor(() => expect(added).toEqual(['reports']))
  expect(sent.deploy).toMatchObject({ image_ref: digest })
  api.restore()
})

test('a name typed by hand is kept when the image changes', () => {
  const view = renderForm({ onAdded: () => {}, onCancel: () => {} })
  fireEvent.change(view.getByLabelText('Image'), { target: { value: IMAGE } })
  fireEvent.change(view.getByLabelText('Service name'), {
    target: { value: 'audit' },
  })
  fireEvent.change(view.getByLabelText('Image'), {
    target: { value: 'registry.example.com/acme/other:2.0.0' },
  })
  expect((view.getByLabelText('Service name') as HTMLInputElement).value).toBe(
    'audit'
  )
})

test('nothing is sent for an image, a name or a port that cannot be one', async () => {
  const { api } = temps()
  const view = renderForm({ onAdded: () => {}, onCancel: () => {} })
  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))
  await view.findByText("Enter the plugin's image.")
  await view.findByText('Name the service the plugin runs as.')

  fireEvent.change(view.getByLabelText('Image'), {
    target: { value: 'two words' },
  })
  fireEvent.change(view.getByLabelText('Service name'), {
    target: { value: 'Not A Slug' },
  })
  fireEvent.change(view.getByLabelText('Port'), { target: { value: '70000' } })
  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))
  await view.findByText('An image reference has no spaces.')
  await view.findByText('Use lowercase letters, digits and hyphens.')
  await view.findByText('A port is between 1 and 65535.')
  expect(api.calls).toEqual([])
  api.restore()
})

test('a step that fails is shown where it failed, with the service kept and a way to try again', async () => {
  const { api } = temps({ status: 'failed' })
  const added: string[] = []
  const view = renderForm({
    onAdded: (name) => added.push(name),
    onCancel: () => {},
  })
  fireEvent.change(view.getByLabelText('Image'), { target: { value: IMAGE } })
  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))

  await view.findByText(/Deployment 42 of reports ended as failed/)
  expect(view.getByRole('button', { name: 'Try again' })).toBeTruthy()
  expect(view.getByRole('link', { name: 'reports' }).getAttribute('href')).toBe(
    '/projects/reports'
  )
  const progress = view.getByRole('status', { name: 'Progress' })
  expect(progress.textContent).toContain('Wait until it is live: Failed')
  expect(progress.textContent).toContain('Add it as a plugin: Not started')
  expect(added).toEqual([])
  expect(api.called('POST', '/x/plugins/services')).toBe(false)
  api.restore()
})

test('a verification request is handed on with a retry that only repeats the last step', async () => {
  const { api } = temps({ refuseAdd: true })
  const handed: Array<() => void> = []
  const view = renderForm({
    onAdded: () => {},
    onCancel: () => {},
    onSensitiveError: (_error, retry) => {
      handed.push(retry)
      return true
    },
  })
  fireEvent.change(view.getByLabelText('Image'), { target: { value: IMAGE } })
  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))
  await waitFor(() => expect(handed).toHaveLength(1))
  await view.findByText('The service did not answer.')

  const before = api.calls.length
  handed[0]()
  await waitFor(() => expect(handed).toHaveLength(2))
  const repeated = api.calls.slice(before)
  expect(repeated).toEqual([{ method: 'POST', path: '/x/plugins/services' }])
  api.restore()
})
