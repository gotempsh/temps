// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type {
  DnsInternalAccess,
  InternalAccessGrantResponse,
  InternalAccessSandboxGrantResponse,
} from '@/api/client/types.gen'
import {
  callerLabel,
  callerState,
  clampGrantsPage,
  defaultGrantLimits,
  exampleAddress,
  grantLimitsSchema,
  grantsLookupComplete,
  internalAccessDiagnostic,
  internalAccessWriteBlock,
  isInherited,
  limitedProjectGrant,
  limitedSandboxGrant,
  oneClickProjectGrant,
  oneClickSandboxGrant,
  pageOf,
  sandboxAccessDiagnostic,
  sandboxCallerLabel,
  sandboxCurl,
  sandboxesForAllowList,
  scopeSummary,
  scopesSummary,
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

test('labels and scope summaries name both scopes', () => {
  expect(callerLabel(grant)).toBe('Checkout worker, all environments')
  expect(scopeSummary(grant)).toBe('all environments')
  const narrow = {
    ...grant,
    caller_environment_id: 20,
    caller_environment_name: 'production',
    target_environment_id: 10,
    target_environment_name: 'staging',
  }
  expect(callerLabel(narrow)).toBe('Checkout worker, production')
  expect(scopeSummary(narrow)).toBe('staging only, from production')
  expect(scopesSummary([grant, narrow, grant])).toBe(
    'all environments; staging only, from production'
  )
})

test('a project-wide grant is inherited on an environment card only', () => {
  expect(isInherited(grant)).toBe(false)
  expect(isInherited(grant, 10)).toBe(true)
  expect(isInherited({ ...grant, target_environment_id: 10 }, 10)).toBe(false)
})

test('one click sends only the caller, plus the environment on its card', () => {
  expect(oneClickProjectGrant(2)).toEqual({ caller_project_id: 2 })
  expect(oneClickProjectGrant(2, 10)).toEqual({
    caller_project_id: 2,
    target_environment_id: 10,
  })
  expect(oneClickSandboxGrant('sbx_alpha')).toEqual({ sandbox_id: 'sbx_alpha' })
  expect(oneClickSandboxGrant('sbx_alpha', 10)).toEqual({
    sandbox_id: 'sbx_alpha',
    target_environment_id: 10,
  })
})

test('Limit… sends the narrower scope and a trimmed reason', () => {
  expect(defaultGrantLimits(10)).toEqual({
    callerEnvironmentId: null,
    targetEnvironmentId: 10,
    description: '',
  })
  const limits = {
    callerEnvironmentId: 20,
    targetEnvironmentId: null,
    description: '  why  ',
  }
  expect(grantLimitsSchema.safeParse(limits).success).toBe(true)
  expect(limitedProjectGrant(2, limits)).toEqual({
    caller_project_id: 2,
    caller_environment_id: 20,
    target_environment_id: null,
    description: 'why',
  })
  // A sandbox has no environments of its own.
  expect(
    limitedSandboxGrant('sbx_alpha', { ...limits, description: '   ' })
  ).toEqual({
    sandbox_id: 'sbx_alpha',
    target_environment_id: null,
    description: null,
  })
  expect(
    grantLimitsSchema.safeParse({ ...limits, description: 'x'.repeat(201) })
      .success
  ).toBe(false)
})

test('a row is allowed by its own grants; on an environment card a project-wide one is inherited', () => {
  const other = { ...grant, id: 4, caller_project_id: 5 }
  const scoped = { ...grant, id: 6, target_environment_id: 10 }
  const mine = (g: InternalAccessGrantResponse) => g.caller_project_id === 2
  expect(callerState([grant, other], mine)).toEqual({
    grants: [grant],
    removable: [grant],
    inherited: false,
  })
  expect(callerState([grant, other], mine, 10)).toEqual({
    grants: [grant],
    removable: [],
    inherited: true,
  })
  expect(callerState([grant, scoped], mine, 10).removable).toEqual([scoped])
  expect(callerState([other], mine).grants).toEqual([])
})

test('a lookup decides rows only when it holds every grant', () => {
  expect(grantsLookupComplete(undefined)).toBe(false)
  expect(grantsLookupComplete({ items: [], total: 0 })).toBe(true)
  expect(grantsLookupComplete({ items: [grant], total: 1 })).toBe(true)
  expect(grantsLookupComplete({ items: [grant], total: 101 })).toBe(false)
})

test('only roles without projects:write are told they cannot change it', () => {
  expect(internalAccessWriteBlock('admin')).toBeNull()
  expect(internalAccessWriteBlock('user')).toBeNull()
  expect(internalAccessWriteBlock('custom')).toBeNull()
  expect(internalAccessWriteBlock(undefined)).toBeNull()
  expect(internalAccessWriteBlock('reader')).toBe(
    'Your role (Reader) cannot change internal access; it needs projects:write.'
  )
  expect(internalAccessWriteBlock('platform_admin')).toContain('Platform admin')
})

test('sandboxes list live ones matching the search, running first', () => {
  const sandbox = (id: string, name: string, status: string) => ({
    id,
    name,
    status,
  })
  const listed = [
    sandbox('sbx_1', 'old scratch', 'stopped'),
    sandbox('sbx_2', 'e2e caller', 'running'),
    sandbox('sbx_3', 'gone', 'destroyed'),
    sandbox('sbx_4', 'e2e builder', 'paused'),
    sandbox('sbx_5', 'agent', 'running'),
  ]
  expect(sandboxesForAllowList(listed, '').map((s) => s.id)).toEqual([
    'sbx_2',
    'sbx_5',
    'sbx_1',
    'sbx_4',
  ])
  expect(sandboxesForAllowList(listed, ' E2E ').map((s) => s.id)).toEqual([
    'sbx_2',
    'sbx_4',
  ])
  expect(sandboxesForAllowList(listed, 'sbx_1').map((s) => s.id)).toEqual([
    'sbx_1',
  ])
  expect(pageOf([1, 2, 3, 4, 5], 2, 2)).toEqual([3, 4])
})

test('the example address is the internal name', () => {
  expect(exampleAddress('orders-api')).toBe(
    'http://production.orders-api.temps.local'
  )
  expect(exampleAddress('orders-api', 'staging')).toBe(
    'http://staging.orders-api.temps.local'
  )
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
