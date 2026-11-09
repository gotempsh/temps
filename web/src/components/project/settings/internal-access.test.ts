// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type {
  DnsInternalAccess,
  InternalAccessGrantResponse,
} from '@/api/client/types.gen'
import {
  allowLabel,
  callerLabel,
  emptyGrantForm,
  exampleAddress,
  internalAccessDiagnostic,
  internalAccessFormSchema,
  isInherited,
  targetLabel,
  toCreateRequest,
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
