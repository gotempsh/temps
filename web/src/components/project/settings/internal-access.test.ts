// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type {
  DnsInternalAccess,
  InternalAccessGrantResponse,
  InternalAccessSandboxGrantResponse,
} from '@/api/client/types.gen'
import {
  allowLabel,
  callerLabel,
  clampGrantsPage,
  emptyGrantForm,
  emptySandboxGrantForm,
  exampleAddress,
  internalAccessDiagnostic,
  internalAccessFormSchema,
  isInherited,
  isSandboxGrantInherited,
  sandboxAccessDiagnostic,
  sandboxCallerLabel,
  sandboxCurl,
  sandboxGrantFormSchema,
  sandboxTargetLabel,
  targetLabel,
  toCreateRequest,
  toCreateSandboxRequest,
} from './internal-access'

const grant: InternalAccessGrantResponse = {
  id: 3,
  target_project_id: 1,
  target_project_name: 'Orders API',
  target_project_slug: 'orders-api',
  target_environment_id: null,
  target_environment_name: null,
  caller_project_id: 2,
  caller_project_name: 'Checkout worker',
  caller_project_slug: 'checkout-worker',
  caller_environment_id: null,
  caller_environment_name: null,
  description: null,
  created_by: 9,
  created_at: '2026-11-09T10:00:00Z',
}

test('labels name both scopes', () => {
  expect(callerLabel(grant)).toBe('Checkout worker, all environments')
  expect(targetLabel(grant)).toBe('Every environment')
  const narrow = {
    ...grant,
    caller_environment_id: 20,
    caller_environment_name: 'production',
    target_environment_id: 10,
    target_environment_name: 'staging',
  }
  expect(callerLabel(narrow)).toBe('Checkout worker, production')
  expect(targetLabel(narrow)).toBe('staging')
})

test('a project-wide grant is inherited on an environment card only', () => {
  expect(isInherited(grant)).toBe(false)
  expect(isInherited(grant, 10)).toBe(true)
  expect(isInherited({ ...grant, target_environment_id: 10 }, 10)).toBe(false)
})

test('the form needs a calling project and sends nulls for whole scopes', () => {
  const empty = emptyGrantForm()
  expect(internalAccessFormSchema.safeParse(empty).success).toBe(false)
  expect(toCreateRequest(empty)).toBeNull()

  const values = { ...empty, callerProjectId: 2, description: '  why  ' }
  expect(internalAccessFormSchema.safeParse(values).success).toBe(true)
  expect(toCreateRequest(values)).toEqual({
    caller_project_id: 2,
    caller_environment_id: null,
    target_environment_id: null,
    description: 'why',
  })
  expect(toCreateRequest({ ...values, description: '   ' })?.description).toBe(
    null
  )
})

test('an environment card targets its own environment', () => {
  expect(emptyGrantForm(10).targetEnvironmentId).toBe(10)
})

test('a description over the limit is refused', () => {
  const values = {
    ...emptyGrantForm(),
    callerProjectId: 2,
    description: 'x'.repeat(201),
  }
  expect(internalAccessFormSchema.safeParse(values).success).toBe(false)
})

test('the example address is the internal name', () => {
  expect(exampleAddress('orders-api')).toBe(
    'http://production.orders-api.temps.local'
  )
  expect(exampleAddress('orders-api', 'staging')).toBe(
    'http://staging.orders-api.temps.local'
  )
  expect(allowLabel('Orders API')).toBe('Allow it to call Orders API')
})

const access: DnsInternalAccess = {
  target_project_id: 1,
  target_project_name: 'Orders API',
  target_project_slug: 'orders-api',
  target_environment_id: 10,
  target_environment_name: 'production',
  reach: 'not_granted',
  setup_path: '/projects/orders-api/settings/security#internal-access',
}

test('the DNS test links to the owner when calls get 403', () => {
  const refused = internalAccessDiagnostic(access, 'the selected project')
  expect(refused?.tone).toBe('error')
  expect(refused?.title).toBe('Calls get 403')
  expect(refused?.setupPath).toBe(access.setup_path)

  const partial = internalAccessDiagnostic(
    { ...access, reach: 'some_environments' },
    'the selected project'
  )
  expect(partial?.tone).toBe('warning')
  expect(partial?.setupPath).toBe(access.setup_path)

  const allowed = internalAccessDiagnostic(
    { ...access, reach: 'granted' },
    'the selected project'
  )
  expect(allowed?.tone).toBe('success')
  expect(allowed?.setupPath).toBeNull()

  expect(
    internalAccessDiagnostic(
      { ...access, reach: 'same_project' },
      'the selected project'
    )
  ).toBeNull()
})

test('a page past the end of the grants goes back to the last page that has any', () => {
  expect(clampGrantsPage(2, 21, 20)).toBe(2)
  // The only grant of page 2 was removed.
  expect(clampGrantsPage(2, 20, 20)).toBe(1)
  expect(clampGrantsPage(5, 41, 20)).toBe(3)
  // No grants left: page 1, which says so.
  expect(clampGrantsPage(3, 0, 20)).toBe(1)
  expect(clampGrantsPage(0, 10, 20)).toBe(1)
})

const sandboxGrant: InternalAccessSandboxGrantResponse = {
  id: 41,
  target_project_id: 1,
  target_project_name: 'Orders API',
  target_project_slug: 'orders-api',
  target_environment_id: null,
  target_environment_name: null,
  sandbox_id: 'sbx_alpha',
  sandbox_name: 'agent scratchpad',
  sandbox_status: 'running',
  description: null,
  created_by: 9,
  created_at: '2026-11-12T10:00:00Z',
}

test('a sandbox grant names the sandbox, and its state when not running', () => {
  expect(sandboxCallerLabel(sandboxGrant)).toBe(
    'Sandbox agent scratchpad · sbx_alpha'
  )
  expect(
    sandboxCallerLabel({ ...sandboxGrant, sandbox_status: 'stopped' })
  ).toBe('Sandbox agent scratchpad · sbx_alpha (stopped)')
  // A sandbox with no name of its own, or named after its id, shows the id once.
  expect(sandboxCallerLabel({ ...sandboxGrant, sandbox_name: '  ' })).toBe(
    'Sandbox sbx_alpha'
  )
  expect(
    sandboxCallerLabel({ ...sandboxGrant, sandbox_name: 'sbx_alpha' })
  ).toBe('Sandbox sbx_alpha')
  expect(sandboxTargetLabel(sandboxGrant)).toBe('Every environment')
  expect(
    sandboxTargetLabel({
      ...sandboxGrant,
      target_environment_id: 4,
      target_environment_name: 'staging',
    })
  ).toBe('staging')
  expect(isSandboxGrantInherited(sandboxGrant, 4)).toBe(true)
  expect(isSandboxGrantInherited(sandboxGrant)).toBe(false)
})

test('the sandbox form needs a sandbox and sends a trimmed reason', () => {
  const empty = emptySandboxGrantForm(7)
  expect(empty).toEqual({
    sandboxId: '',
    targetEnvironmentId: 7,
    description: '',
  })
  expect(sandboxGrantFormSchema.safeParse(empty).success).toBe(false)
  expect(toCreateSandboxRequest(empty)).toBeNull()
  expect(
    toCreateSandboxRequest({
      sandboxId: 'sbx_alpha',
      targetEnvironmentId: null,
      description: '  agent tests the API ',
    })
  ).toEqual({
    sandbox_id: 'sbx_alpha',
    target_environment_id: null,
    description: 'agent tests the API',
  })
  expect(
    toCreateSandboxRequest({
      sandboxId: 'sbx_alpha',
      targetEnvironmentId: 3,
      description: '   ',
    })?.description
  ).toBeNull()
})

test('the sandbox curl line is plain http on the internal name', () => {
  expect(sandboxCurl('orders-api', 'staging')).toBe(
    'curl -sS http://staging.orders-api.temps.local/'
  )
})

test('the DNS test tool tells a sandbox where its grant is missing', () => {
  const access: DnsInternalAccess = {
    target_project_id: 1,
    target_project_name: 'Orders API',
    target_project_slug: 'orders-api',
    target_environment_id: 4,
    target_environment_name: 'production',
    reach: 'not_granted',
    setup_path: '/projects/orders-api/settings/security#internal-access',
  }
  const refused = sandboxAccessDiagnostic(access, 'sbx_alpha')
  expect(refused.tone).toBe('error')
  expect(refused.detail).toContain('sandbox sbx_alpha')
  expect(refused.setupPath).toBe(access.setup_path)
  const allowed = sandboxAccessDiagnostic(
    { ...access, reach: 'granted' },
    'sbx_alpha'
  )
  expect(allowed.tone).toBe('success')
  expect(allowed.setupPath).toBeNull()
})
