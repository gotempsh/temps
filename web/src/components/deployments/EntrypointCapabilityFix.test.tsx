// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ContainerSecurityResponse, SuggestedFix } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EntrypointCapabilityFix } from './EntrypointCapabilityFix'

installBrowserWindow()
afterEach(cleanup)

const PATH = '/projects/4/container-security'
const ACTION = 'Allow NET_BIND_SERVICE for this project and redeploy'

const fix: SuggestedFix = {
  required_capabilities: ['NET_BIND_SERVICE'],
  verified: true,
  allow_capabilities: ['NET_BIND_SERVICE'],
  not_allowed_capabilities: [],
  setting: 'allowed_capabilities',
  setting_path: PATH,
  image_fix:
    'Remove the file capabilities from the entrypoint binary in the image (`setcap -r <binary>`) and listen on a port above 1024.',
}

function security(
  overrides: Partial<ContainerSecurityResponse> = {}
): ContainerSecurityResponse {
  return {
    allowed_capabilities: [],
    options: [
      {
        id: 'NET_BIND_SERVICE',
        label: 'Allow binding ports below 1024 (NET_BIND_SERVICE)',
        description: 'Lets a setcap entrypoint start.',
        trade_off: 'May listen on privileged ports.',
      },
    ],
    always_enforced: ['Every Linux capability not allowed here is dropped'],
    can_edit: true,
    edit_requirement:
      'An instance administrator with projects:write on this project can change container security.',
    ...overrides,
  }
}

/** The dialog's confirm button: the last of the two with the action's name. */
function lastAction(buttons: HTMLElement[]): HTMLElement {
  return buttons[buttons.length - 1]
}

function renderFix(props: {
  onRedeploy?: () => Promise<void>
  suggested?: SuggestedFix
}) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <MemoryRouter>
        <EntrypointCapabilityFix
          projectId={4}
          fix={props.suggested ?? fix}
          settingsHref="/projects/app/settings/security#container-security"
          onRedeploy={props.onRedeploy}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('confirming allows the capability, audited server-side, then redeploys the same source', async () => {
  const events: string[] = []
  const bodies: unknown[] = []
  mockApi({
    [`GET ${PATH}`]: () => jsonResponse(security()),
    [`PUT ${PATH}`]: async (request) => {
      bodies.push(await request.json())
      events.push('saved')
      return jsonResponse(
        security({ allowed_capabilities: ['NET_BIND_SERVICE'] })
      )
    },
  })
  const screen = renderFix({
    onRedeploy: async () => {
      events.push('redeployed')
    },
  })

  fireEvent.click(await screen.findByRole('button', { name: ACTION }))
  // The dialog says what changes, the trade-off and the image alternative.
  expect(await screen.findByText(/Trade-off:/)).toBeTruthy()
  expect(screen.getByText(/Keep full hardening instead/)).toBeTruthy()
  const confirm = lastAction(
    screen.getAllByRole('button', { name: ACTION })
  ) as HTMLButtonElement
  expect(confirm.disabled).toBe(true)
  fireEvent.click(screen.getByRole('checkbox'))
  expect(confirm.disabled).toBe(false)
  fireEvent.click(confirm)

  await waitFor(() => expect(events).toEqual(['saved', 'redeployed']))
  expect(bodies).toEqual([
    {
      allowed_capabilities: ['NET_BIND_SERVICE'],
      expected_allowed_capabilities: [],
      acknowledge_risks: true,
    },
  ])
})

test('nothing is changed until the person confirms', async () => {
  const api = mockApi({ [`GET ${PATH}`]: () => jsonResponse(security()) })
  let redeployed = false
  const screen = renderFix({
    onRedeploy: async () => {
      redeployed = true
    },
  })
  fireEvent.click(await screen.findByRole('button', { name: ACTION }))
  fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }))
  expect(api.called('PUT', PATH)).toBe(false)
  expect(redeployed).toBe(false)
})

test('without permission the card says who can allow it and offers no action', async () => {
  mockApi({
    [`GET ${PATH}`]: () => jsonResponse(security({ can_edit: false })),
  })
  const screen = renderFix({ onRedeploy: async () => {} })
  expect(
    await screen.findByText(/An instance administrator with projects:write/)
  ).toBeTruthy()
  expect(screen.queryByRole('button', { name: ACTION })).toBeNull()
  expect(screen.getByRole('link', { name: 'Container security' })).toBeTruthy()
})

test('when the capability is already allowed it only redeploys', async () => {
  const api = mockApi({
    [`GET ${PATH}`]: () =>
      jsonResponse(security({ allowed_capabilities: ['NET_BIND_SERVICE'] })),
  })
  let redeployed = false
  const screen = renderFix({
    onRedeploy: async () => {
      redeployed = true
    },
  })
  fireEvent.click(await screen.findByRole('button', { name: 'Redeploy' }))
  await waitFor(() => expect(redeployed).toBe(true))
  expect(api.called('PUT', PATH)).toBe(false)
})

test('a capability no project can allow offers only the image fix', async () => {
  const api = mockApi({})
  const screen = renderFix({
    suggested: {
      ...fix,
      required_capabilities: ['NET_BIND_SERVICE', 'NET_RAW'],
      allow_capabilities: [],
      not_allowed_capabilities: ['NET_RAW'],
    },
  })
  expect(screen.getByText(/which no project/)).toBeTruthy()
  expect(screen.getByText(/setcap -r/)).toBeTruthy()
  expect(screen.queryByRole('button')).toBeNull()
  expect(api.calls).toEqual([])
})

test('a save that needs recent MFA asks for it instead of failing', async () => {
  mockApi({
    [`GET ${PATH}`]: () => jsonResponse(security()),
    [`PUT ${PATH}`]: () =>
      jsonResponse(
        {
          title: 'Verification required',
          status: 428,
          error_code: 'STEP_UP_REQUIRED',
          mfa_setup_required: false,
        },
        428
      ),
  })
  let redeployed = false
  const screen = renderFix({
    onRedeploy: async () => {
      redeployed = true
    },
  })
  fireEvent.click(await screen.findByRole('button', { name: ACTION }))
  fireEvent.click(await screen.findByRole('checkbox'))
  fireEvent.click(
    lastAction(screen.getAllByRole('button', { name: ACTION })) as HTMLElement
  )
  expect(
    await screen.findByRole('dialog', { name: /Verify this sensitive action/ })
  ).toBeTruthy()
  expect(redeployed).toBe(false)
})
