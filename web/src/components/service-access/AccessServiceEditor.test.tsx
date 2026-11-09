// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import type { ReactNode } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import type {
  AccessServiceResponse,
  CreateAccessServiceData,
  RequestServiceGrant,
} from '@/api/client'
import { AccessServiceEditor } from './AccessServiceEditor'
import { ServiceGrantEditor } from './ServiceGrantEditor'

installBrowserWindow()
afterEach(cleanup)
const service: AccessServiceResponse = {
  id: 'svc_test',
  project_id: 7,
  name: 'API',
  policy: {
    origin: 'https://api.example.com',
    path_prefixes: ['/v1'],
    methods: ['GET'],
  },
  enabled: true,
  has_credential: true,
  max_ttl_seconds: 120,
  version: 4,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
function provider(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false }, queries: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>{children}</MemoryRouter>
    </QueryClientProvider>
  )
}
async function click(label: string) {
  await act(async () => {
    fireEvent.click(within(document.body).getByRole('button', { name: label }))
  })
}
async function editName() {
  await act(async () => {
    fireEvent.change(within(document.body).getByLabelText('Name'), {
      target: { value: 'Updated API' },
    })
  })
}
test('saving an edit sends the pinned version without resending a stored credential', async () => {
  let submitted: CreateAccessServiceData['body'] | undefined
  let saved = 0
  const api = mockApi({
    'PUT /projects/7/access-services/svc_test': async (request) => {
      submitted = (await request.json()) as CreateAccessServiceData['body']
      return jsonResponse({ ...service, name: 'Updated API', version: 5 })
    },
  })
  render(
    provider(
      <AccessServiceEditor
        projectId={7}
        service={service}
        onClose={() => {}}
        onSaved={() => {
          saved += 1
        }}
      />
    )
  )
  await editName()
  await click('Save service')
  await waitFor(() => expect(saved).toBe(1))
  expect(submitted?.expected_version).toBe(4)
  expect(submitted?.credential).toBeUndefined()
  expect(submitted?.name).toBe('Updated API')
  api.restore()
})
test('a version conflict stays visible and does not report a successful save', async () => {
  let saved = 0
  const api = mockApi({
    'PUT /projects/7/access-services/svc_test': () =>
      jsonResponse(
        { detail: 'The service changed; reload before saving.' },
        409
      ),
  })
  render(
    provider(
      <AccessServiceEditor
        projectId={7}
        service={service}
        onClose={() => {}}
        onSaved={() => {
          saved += 1
        }}
      />
    )
  )
  await editName()
  await click('Save service')
  await waitFor(() =>
    expect(document.body.textContent).toContain(
      'The service changed; reload before saving.'
    )
  )
  expect(saved).toBe(0)
  api.restore()
})
test('request editor defaults to the service limit and saves the selected policy version', async () => {
  let submitted: RequestServiceGrant | undefined
  let saved = 0
  const api = mockApi({
    'POST /projects/7/sandboxes/sbx_test/service-grants': async (request) => {
      submitted = (await request.json()) as RequestServiceGrant
      return jsonResponse({ id: 'grant_test', connected: false }, 201)
    },
  })
  render(
    provider(
      <ServiceGrantEditor
        projectId={7}
        sandboxId="sbx_test"
        service={service}
        onClose={() => {}}
        onSaved={() => {
          saved += 1
        }}
      />
    )
  )
  expect(
    (
      within(document.body).getByLabelText(
        'Lifetime (seconds)'
      ) as HTMLInputElement
    ).value
  ).toBe('120')
  await click('Save access request')
  await waitFor(() => expect(saved).toBe(1))
  expect(submitted).toEqual({
    service_id: 'svc_test',
    service_version: 4,
    ttl_seconds: 120,
  })
  api.restore()
})
test('request failures explain the missing permission without closing the form', async () => {
  let closed = false
  const api = mockApi({
    'POST /projects/7/sandboxes/sbx_test/service-grants': () =>
      jsonResponse({ detail: 'Sandbox write permission is required.' }, 403),
  })
  render(
    provider(
      <ServiceGrantEditor
        projectId={7}
        sandboxId="sbx_test"
        service={service}
        onClose={() => {
          closed = true
        }}
        onSaved={() => {}}
      />
    )
  )
  await click('Save access request')
  await waitFor(() =>
    expect(document.body.textContent).toContain(
      'Sandbox write permission is required.'
    )
  )
  expect(closed).toBe(false)
  api.restore()
})

test.each(['service', 'grant'] as const)(
  '%s editor prevents edits while saving and restores them after failure',
  async (kind) => {
    let respond: (response: Response) => void = () => {}
    const pendingResponse = new Promise<Response>((resolve) => {
      respond = resolve
    })
    const route =
      kind === 'service'
        ? 'PUT /projects/7/access-services/svc_test'
        : 'POST /projects/7/sandboxes/sbx_test/service-grants'
    const api = mockApi({ [route]: () => pendingResponse })
    let closed = false
    const callbacks = {
      onClose: () => {
        closed = true
      },
      onSaved: () => {},
    }
    try {
      render(
        provider(
          kind === 'service' ? (
            <AccessServiceEditor
              projectId={7}
              service={service}
              {...callbacks}
            />
          ) : (
            <ServiceGrantEditor
              projectId={7}
              sandboxId="sbx_test"
              service={service}
              {...callbacks}
            />
          )
        )
      )
      if (kind === 'service') await editName()
      const input = within(document.body).getByLabelText(
        kind === 'service' ? 'Name' : 'Lifetime (seconds)'
      ) as HTMLInputElement
      const value = input.value
      await click(kind === 'service' ? 'Save service' : 'Save access request')
      await waitFor(() =>
        expect(input.closest('fieldset')?.disabled).toBe(true)
      )
      expect(closed).toBe(false)
      await act(async () => {
        respond(
          jsonResponse({ detail: 'Save failed; retry your changes.' }, 409)
        )
      })
      await waitFor(() =>
        expect(input.closest('fieldset')?.disabled).toBe(false)
      )
      expect(input.value).toBe(value)
      expect(closed).toBe(false)
      expect(document.body.textContent).toContain(
        'Save failed; retry your changes.'
      )
    } finally {
      respond(jsonResponse({}, 500))
      api.restore()
    }
  }
)

test.each(['service', 'grant'] as const)(
  '%s validation shows a field-labelled summary and recovers when corrected',
  async (kind) => {
    render(
      provider(
        kind === 'service' ? (
          <AccessServiceEditor
            projectId={7}
            service={service}
            onClose={() => {}}
            onSaved={() => {}}
          />
        ) : (
          <ServiceGrantEditor
            projectId={7}
            sandboxId="sbx_test"
            service={service}
            onClose={() => {}}
            onSaved={() => {}}
          />
        )
      )
    )
    const input = within(document.body).getByLabelText(
      kind === 'service' ? 'Name' : 'Lifetime (seconds)'
    ) as HTMLInputElement
    await act(async () => {
      fireEvent.change(input, {
        target: { value: kind === 'service' ? '' : '0' },
      })
      fireEvent.submit(input.closest('form')!)
    })
    await waitFor(() =>
      expect(document.body.textContent).toContain('Fix 1 field before saving')
    )
    expect(document.body.textContent).toContain(
      kind === 'service' ? 'Name:' : 'Lifetime (seconds):'
    )
    await act(async () => {
      fireEvent.change(input, {
        target: { value: kind === 'service' ? 'Valid API' : '60' },
      })
    })
    await waitFor(() =>
      expect(document.body.textContent).not.toContain(
        'Fix 1 field before saving'
      )
    )
  }
)

test('a save finishing after leaving refreshes data without navigating', async () => {
  let resolve: (response: Response) => void = () => {}
  const response = new Promise<Response>((done) => {
    resolve = done
  })
  const api = mockApi({
    'PUT /projects/7/access-services/svc_test': () => response,
  })
  let saved = 0
  let closed = 0
  try {
    const view = render(
      provider(
        <AccessServiceEditor
          projectId={7}
          service={service}
          onClose={() => {
            closed++
          }}
          onSaved={() => {
            saved++
          }}
        />
      )
    )
    await editName()
    await click('Save service')
    view.unmount()
    await act(async () => {
      resolve(jsonResponse(service))
      await response
    })
    expect(closed).toBe(0)
    expect(saved).toBe(1)
  } finally {
    api.restore()
  }
})

test('invalid path prefixes stay beside the field and block the request until corrected', async () => {
  const api = mockApi({
    'PUT /projects/7/access-services/svc_test': () => jsonResponse(service),
  })
  let saved = false
  try {
    render(
      provider(
        <AccessServiceEditor
          projectId={7}
          service={service}
          onClose={() => {}}
          onSaved={() => {
            saved = true
          }}
        />
      )
    )
    await act(async () => {
      fireEvent.change(
        within(document.body).getByLabelText('Allowed path prefixes'),
        { target: { value: '/v1/*' } }
      )
    })
    await click('Save service')
    expect(api.called('PUT', '/projects/7/access-services/svc_test')).toBe(
      false
    )
    expect(document.body.textContent).toContain(
      'without encoding, traversal, query strings or wildcards'
    )
    expect(document.body.textContent).toContain('Allowed path prefixes:')
    await act(async () => {
      fireEvent.change(
        within(document.body).getByLabelText('Allowed path prefixes'),
        { target: { value: '/v2' } }
      )
    })
    await click('Save service')
    await waitFor(() => expect(saved).toBe(true))
  } finally {
    api.restore()
  }
})

test('GitHub clone/fetch saves a project-bound source without a bearer credential', async () => {
  let submitted: CreateAccessServiceData['body'] | undefined
  const api = mockApi({
    'GET /projects/7': () =>
      jsonResponse({
        id: 7,
        git_provider_connection_id: 9,
        git_provider_type: 'github',
        repo_owner: 'owner',
        repo_name: 'repo',
      }),
    'POST /projects/7/access-services': async (request) => {
      submitted = (await request.json()) as CreateAccessServiceData['body']
      return jsonResponse(service, 201)
    },
  })
  render(
    provider(
      <AccessServiceEditor
        projectId={7}
        canUseProjectGithub
        onClose={() => {}}
        onSaved={() => {}}
      />
    )
  )
  await click('GitHub clone/fetch')
  await waitFor(() =>
    expect(
      (within(document.body).getByLabelText('Name') as HTMLInputElement).value
    ).toBe('GitHub clone/fetch')
  )
  expect(
    (within(document.body).getByLabelText('HTTPS origin') as HTMLInputElement)
      .readOnly
  ).toBe(true)
  await click('Save service')
  await waitFor(() => expect(submitted).toBeDefined())
  expect(submitted?.policy.github_repository).toEqual({
    connection_id: 9,
    owner: 'owner',
    repository: 'repo',
    protocol: 'git',
  })
  expect(submitted?.policy.path_prefixes).toEqual([
    '/owner/repo',
    '/owner/repo.git',
  ])
  expect(submitted?.policy.methods).toEqual(['GET', 'POST'])
  expect(submitted?.credential).toBeUndefined()
  api.restore()
})

test('editing a GitHub service preserves its connection reference', async () => {
  const github = {
    connection_id: 9,
    owner: 'owner',
    repository: 'repo',
    protocol: 'api' as const,
  }
  let submitted: CreateAccessServiceData['body'] | undefined
  const api = mockApi({
    'PUT /projects/7/access-services/svc_test': async (request) => {
      submitted = (await request.json()) as CreateAccessServiceData['body']
      return jsonResponse(service)
    },
  })
  render(
    provider(
      <AccessServiceEditor
        projectId={7}
        canUseProjectGithub
        service={{
          ...service,
          policy: {
            origin: 'https://api.github.com',
            path_prefixes: ['/repos/owner/repo'],
            methods: ['GET', 'HEAD'],
            github_repository: github,
          },
        }}
        onClose={() => {}}
        onSaved={() => {}}
      />
    )
  )
  await editName()
  await click('Save service')
  await waitFor(() =>
    expect(submitted?.policy.github_repository).toEqual(github)
  )
  expect(submitted?.credential).toBeUndefined()
  api.restore()
})

test('missing GitHub configuration preserves the draft and explains how to connect it', async () => {
  const api = mockApi({
    'GET /projects/7': () => jsonResponse({ id: 7, repo_owner: null }),
  })
  try {
    render(
      provider(
        <AccessServiceEditor
          projectId={7}
          canUseProjectGithub
          service={service}
          onClose={() => {}}
          onSaved={() => {}}
        />
      )
    )
    await editName()
    await click('GitHub API reads')
    await waitFor(() =>
      expect(document.body.textContent).toContain(
        'Connect this project to GitHub, then try again.'
      )
    )
    expect(
      (within(document.body).getByLabelText('Name') as HTMLInputElement).value
    ).toBe('Updated API')
    expect(
      (within(document.body).getByLabelText('HTTPS origin') as HTMLInputElement)
        .value
    ).toBe(service.policy.origin)
    expect(api.called('PUT', '/projects/7/access-services/svc_test')).toBe(
      false
    )
  } finally {
    api.restore()
  }
})

test('GitHub source actions explain and enforce missing repository permission', async () => {
  const api = mockApi({})
  try {
    render(
      provider(
        <AccessServiceEditor
          projectId={7}
          service={service}
          onClose={() => {}}
          onSaved={() => {}}
        />
      )
    )
    expect(
      (
        within(document.body).getByRole('button', {
          name: 'GitHub API reads',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    expect(
      (
        within(document.body).getByRole('button', {
          name: 'GitHub clone/fetch',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    expect(document.body.textContent).toContain('repository read')
    expect(api.called('GET', '/projects/7')).toBe(false)
  } finally {
    api.restore()
  }
})
