// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import { getDeploymentOptions } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { RedeploymentModal } from './RedeploymentModal'

installBrowserWindow()
afterEach(cleanup)

const project = {
  id: 7,
  slug: 'example',
  source_type: 'docker_image',
} as ProjectResponse

const environments = [
  { id: 1, name: 'production', slug: 'production', current_deployment_id: 11 },
  { id: 2, name: 'staging', slug: 'staging', current_deployment_id: 22 },
  { id: 3, name: 'idle', slug: 'idle', current_deployment_id: null },
].map((environment) => ({ ...environment, is_preview: false }))

const running = (image: string) => () =>
  jsonResponse({ id: 1, metadata: { externalImageRef: image } })

function renderModal(
  defaultEnvironment: number,
  staging = running('registry.example.test/app:staging'),
  options: {
    /** What an earlier visit left in the cache for staging's deployment. */
    cachedImage?: string
    mode?: 'new' | 'redeploy'
    imageRef?: string
    imageFromEnvironment?: boolean
  } = {}
) {
  const onConfirm = mock(async () => {})
  mockApi({
    'GET /projects/7/environments': () => jsonResponse(environments),
    'GET /projects/7/deployments/11': running(
      'registry.example.test/app:production'
    ),
    'GET /projects/7/deployments/22': staging,
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  if (options.cachedImage) {
    // After mockApi: the query key includes the client's base URL.
    client.setQueryData(
      getDeploymentOptions({ path: { project_id: 7, deployment_id: 22 } })
        .queryKey,
      {
        id: 22,
        metadata: { externalImageRef: options.cachedImage },
      } as DeploymentResponse
    )
  }
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <RedeploymentModal
          project={project}
          isOpen
          onClose={() => {}}
          onConfirm={onConfirm}
          defaultEnvironment={defaultEnvironment}
          mode={options.mode}
          imageRef={
            'imageRef' in options
              ? options.imageRef
              : 'registry.example.test/app:last-deployed'
          }
          imageFromEnvironment={options.imageFromEnvironment ?? true}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const image = () => view.getByLabelText('Image reference') as HTMLInputElement
  return { view, image }
}

test('a redeploy starts on the image the chosen environment runs', async () => {
  const { image } = renderModal(2)

  await waitFor(() =>
    expect(image().value).toBe('registry.example.test/app:staging')
  )
})

test('an environment running nothing falls back to the last deployed image', async () => {
  const { view, image } = renderModal(3)

  await view.findByRole('button', { name: /deploy/i })
  await waitFor(() =>
    expect(image().value).toBe('registry.example.test/app:last-deployed')
  )
})

test('what the user typed survives the running image arriving', async () => {
  const { image } = renderModal(2)

  fireEvent.change(image(), {
    target: { value: 'registry.example.test/app:next' },
  })
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(image().value).toBe('registry.example.test/app:next')
})

test('an unknown running image is said out loud rather than assumed', async () => {
  const { view, image } = renderModal(2, () =>
    jsonResponse({ title: 'Unavailable' }, 500)
  )

  await view.findByText(/Could not load the image this environment runs/)
  expect(image().value).toBe('registry.example.test/app:last-deployed')
})

test('a running deployment with no image is said out loud rather than assumed', async () => {
  const { view, image } = renderModal(2, () =>
    jsonResponse({ id: 22, metadata: {} })
  )

  await view.findByText(/What this environment runs now records no image/)
  expect(image().value).toBe('registry.example.test/app:last-deployed')
  expect(view.getByText(/Check it before deploying/)).toBeTruthy()
})

test('with no image to fall back on, it asks for one instead of claiming a last image', async () => {
  const { view } = renderModal(
    2,
    () => jsonResponse({ id: 22, metadata: {} }),
    {
      imageRef: undefined,
    }
  )

  await view.findByText(/so enter the image to deploy/)
  expect(view.queryByText(/last deployed image/)).toBeNull()
})

test('redeploying an older image ignores the running image in the cache', async () => {
  const { view, image } = renderModal(2, undefined, {
    cachedImage: 'registry.example.test/app:new',
    mode: 'redeploy',
    imageRef: 'registry.example.test/app:older',
    imageFromEnvironment: false,
  })

  // The environment's name means its running deployment is known by now.
  await view.findByText('staging')
  expect(image().value).toBe('registry.example.test/app:older')
  expect(view.queryByText(/records no image/)).toBeNull()
})
