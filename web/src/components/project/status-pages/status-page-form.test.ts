// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import type {
  EnvironmentResponse,
  MonitorResponse,
  StatusPageResponse,
} from '@/api/client'
import {
  addDomainSchema,
  appearanceFormSchema,
  buildMonitorChoices,
  choiceLabel,
  componentRow,
  componentRowsFromPage,
  createDefaults,
  createStatusPageSchema,
  customIncidentBody,
  customIncidentSchema,
  groupNames,
  incidentTargets,
  normalizeAccent,
  suggestPublicName,
  daysUntil,
  domainActions,
  domainStatusLabel,
  firstAvailableChoice,
  incidentUpdateSchema,
  initialIncidentStatus,
  MAX_GROUP_NAME_CHARS,
  MAX_INCIDENT_UPDATE_CHARS,
  operationQueries,
  statusPageFormSchema,
  switchComponent,
  toSelection,
  type ComponentRow,
} from './status-page-form'

function monitor(id: number, name: string): MonitorResponse {
  return {
    id,
    name,
    project_id: 1,
    environment_id: 1,
    monitor_type: 'web',
    monitor_url: `https://internal-${id}.example.test/health`,
    check_interval_seconds: 60,
    is_active: true,
    is_managed: false,
    created_at: '2026-10-01T00:00:00Z',
    updated_at: '2026-10-01T00:00:00Z',
  }
}

/** A page showing two projects: Billing (one component) and Web (two). */
const page = {
  projects: [
    { id: 7, name: 'Billing', slug: 'billing' },
    { id: 9, name: 'Web', slug: 'web' },
  ],
  monitors: [
    {
      monitor_id: 3,
      monitor_name: 'web-staging',
      display_name: 'Staging',
      group_name: 'Cloud',
      position: 2,
      project_id: 9,
      project_name: 'Web',
      environment_id: 31,
      environment_name: 'staging',
    },
    {
      monitor_id: 2,
      monitor_name: 'billing-production',
      display_name: 'Billing',
      group_name: null,
      position: 0,
      project_id: 7,
      project_name: 'Billing',
      environment_id: 21,
      environment_name: 'production',
    },
    {
      monitor_id: 4,
      monitor_name: 'web-production',
      display_name: 'Dashboard',
      group_name: 'Cloud',
      position: 1,
      project_id: 9,
      project_name: 'Web',
      environment_id: 30,
      environment_name: 'production',
    },
  ],
} as Pick<StatusPageResponse, 'monitors' | 'projects'>

function row(overrides: Partial<ComponentRow> = {}): ComponentRow {
  return {
    monitor_id: 1,
    monitor_name: 'x',
    project_id: 7,
    project_name: 'Billing',
    environment_name: 'production',
    display_name: 'API',
    group_name: '',
    ...overrides,
  }
}

describe('componentRowsFromPage', () => {
  it('lists components in page order with their project and group', () => {
    const rows = componentRowsFromPage(page)
    expect(
      rows.map((r) => [r.monitor_id, r.project_name, r.group_name])
    ).toEqual([
      [2, 'Billing', ''],
      [4, 'Web', 'Cloud'],
      [3, 'Web', 'Cloud'],
    ])
  })
})

describe('toSelection', () => {
  it('trims names and sends a blank group as no group', () => {
    expect(
      toSelection([
        row({ monitor_id: 1, display_name: ' API ', group_name: ' Cloud ' }),
        row({ monitor_id: 2, display_name: 'Web', group_name: '   ' }),
      ])
    ).toEqual([
      { monitor_id: 1, display_name: 'API', group_name: 'Cloud' },
      { monitor_id: 2, display_name: 'Web', group_name: null },
    ])
  })
})

describe('groupNames', () => {
  it('offers each group once, in order of first use', () => {
    expect(
      groupNames([
        { group_name: 'Cloud' },
        { group_name: '' },
        { group_name: ' Cloud ' },
        { group_name: 'Email' },
      ])
    ).toEqual(['Cloud', 'Email'])
  })
})

describe('statusPageFormSchema', () => {
  it('requires a public name for every component', () => {
    const result = statusPageFormSchema.safeParse({
      components: [row({ display_name: '  ' })],
    })
    expect(result.success).toBe(false)
    expect(result.error?.issues[0].path).toEqual([
      'components',
      0,
      'display_name',
    ])
  })

  it('limits group names to the server limit', () => {
    const result = statusPageFormSchema.safeParse({
      components: [row({ group_name: 'x'.repeat(MAX_GROUP_NAME_CHARS + 1) })],
    })
    expect(result.error?.issues[0].path).toEqual([
      'components',
      0,
      'group_name',
    ])
  })

  it('needs at least one component', () => {
    const result = statusPageFormSchema.safeParse({ components: [] })
    expect(result.error?.issues[0].message).toBe('Add at least one component')
  })

  it('accepts components from several projects', () => {
    expect(
      statusPageFormSchema.safeParse({
        components: [
          row({ monitor_id: 1 }),
          row({ monitor_id: 2, project_id: 9, project_name: 'Web' }),
        ],
      }).success
    ).toBe(true)
  })
})

describe('operationQueries', () => {
  it('matches every cached variant of a generated SDK query key', async () => {
    const client = new QueryClient()
    const detail = [
      { _id: 'getStatusPage', baseUrl: '/api', path: { status_page_id: 1 } },
    ]
    const other = [
      { _id: 'listStatusPages', baseUrl: '/api', query: { page: 1 } },
    ]
    client.setQueryData(detail, { id: 1 })
    client.setQueryData(other, { items: [] })
    await client.invalidateQueries(operationQueries('getStatusPage'))
    expect(client.getQueryState(detail)?.isInvalidated).toBe(true)
    expect(client.getQueryState(other)?.isInvalidated).toBe(false)
  })
})

describe('incidentUpdateSchema', () => {
  it('requires a message and enforces the server limit', () => {
    const base = { status: 'identified' as const, is_public: true }
    expect(
      incidentUpdateSchema.safeParse({ ...base, message: '  ' }).success
    ).toBe(false)
    expect(
      incidentUpdateSchema.safeParse({
        ...base,
        message: 'x'.repeat(MAX_INCIDENT_UPDATE_CHARS + 1),
      }).success
    ).toBe(false)
    expect(
      incidentUpdateSchema.safeParse({
        ...base,
        message: 'x'.repeat(MAX_INCIDENT_UPDATE_CHARS),
      }).success
    ).toBe(true)
  })

  it('falls back to investigating for an unknown incident status', () => {
    expect(initialIncidentStatus('monitoring')).toBe('monitoring')
    expect(initialIncidentStatus('something-else')).toBe('investigating')
  })
})

describe('addDomainSchema', () => {
  it('accepts a hostname and refuses URLs, wildcards and bare labels', () => {
    const ok = (hostname: string) =>
      addDomainSchema.safeParse({ hostname, verification_method: 'http-01' })
        .success
    expect(ok('status.example.com')).toBe(true)
    expect(ok('status.example.com.')).toBe(true)
    expect(ok('https://status.example.com')).toBe(false)
    expect(ok('status.example.com/path')).toBe(false)
    expect(ok('*.example.com')).toBe(false)
    expect(ok('localhost')).toBe(false)
    expect(ok('')).toBe(false)
  })
})

describe('domainStatusLabel', () => {
  it('distinguishes serving from verified-but-unpublished', () => {
    expect(domainStatusLabel({ status: 'active', serving: true })).toBe(
      'Serving'
    )
    expect(domainStatusLabel({ status: 'active', serving: false })).toContain(
      'unpublished'
    )
    expect(domainStatusLabel({ status: 'pending', serving: false })).toBe(
      'Waiting for DNS'
    )
  })
})

describe('domain renewal states', () => {
  it('labels renewals as still serving', () => {
    expect(
      domainStatusLabel({ status: 'renewal_pending', serving: true })
    ).toContain('renewal in progress')
    expect(
      domainStatusLabel({ status: 'renewal_failed', serving: true })
    ).toContain('renewal failed')
  })

  it('offers renew for a held certificate and verify to finish an open order', () => {
    expect(domainActions({ status: 'active' }, true)).toEqual({
      verify: false,
      renew: true,
    })
    expect(domainActions({ status: 'renewal_failed' }, true)).toEqual({
      verify: false,
      renew: true,
    })
    expect(domainActions({ status: 'renewal_pending' }, true)).toEqual({
      verify: true,
      renew: false,
    })
    expect(domainActions({ status: 'unverified' }, true)).toEqual({
      verify: true,
      renew: false,
    })
  })

  it('offers nothing on an unpublished page, which holds no certificates', () => {
    expect(domainActions({ status: 'unverified' }, false)).toEqual({
      verify: false,
      renew: false,
    })
  })

  it('counts whole days to expiry', () => {
    const now = new Date('2026-10-12T00:00:00Z')
    expect(daysUntil('2026-10-22T12:00:00Z', now)).toBe(10)
    expect(daysUntil(null, now)).toBeNull()
    expect(daysUntil('not a date', now)).toBeNull()
  })
})

function environment(
  id: number,
  name: string,
  isPreview = false
): EnvironmentResponse {
  return { id, name, is_preview: isPreview } as EnvironmentResponse
}

function envMonitor(
  id: number,
  environmentId: number | null,
  managed: boolean
): MonitorResponse {
  return {
    ...monitor(id, `m${id}`),
    environment_id: environmentId,
    is_managed: managed,
  }
}

describe('buildMonitorChoices', () => {
  it('offers production first, then previews, then project-level monitors, managed first', () => {
    const choices = buildMonitorChoices(
      [environment(1, 'preview-1', true), environment(2, 'production')],
      [
        envMonitor(12, null, true),
        envMonitor(13, 1, true),
        envMonitor(10, 2, false),
        envMonitor(11, 2, true),
      ]
    )
    expect(
      choices.map((c) => [c.monitor.id, c.environment_name, c.is_preview])
    ).toEqual([
      [11, 'production', false],
      [10, 'production', false],
      [13, 'preview-1', true],
      [12, null, false],
    ])
  })
})

describe('componentRow / createDefaults', () => {
  const project = { id: 7, name: 'billing-api' }
  const [production, preview] = buildMonitorChoices(
    [environment(1, 'production'), environment(2, 'pr-12', true)],
    [envMonitor(20, 1, true), envMonitor(21, 2, true)]
  )

  it('names a production component after its project, anything else after both', () => {
    expect(componentRow(project, production)).toMatchObject({
      monitor_id: 20,
      project_id: 7,
      project_name: 'billing-api',
      environment_name: 'production',
      display_name: 'Billing Api',
      group_name: '',
    })
    expect(componentRow(project, preview, 'Cloud')).toMatchObject({
      monitor_id: 21,
      display_name: 'Billing Api M21',
      group_name: 'Cloud',
    })
  })

  it('starts with no components and requires one', () => {
    const values = createDefaults('Acme status', [])
    expect(values.publish).toBe(true)
    const result = createStatusPageSchema.safeParse(values)
    expect(result.success).toBe(false)
    expect(result.error?.issues[0].message).toBe('Add at least one component')
    expect(
      createStatusPageSchema.safeParse({
        ...values,
        components: [componentRow(project, production)],
      }).success
    ).toBe(true)
  })
})

describe('environment dropdown helpers', () => {
  const choices = buildMonitorChoices(
    [environment(1, 'production'), environment(2, 'pr-12', true)],
    [
      envMonitor(20, 1, true),
      envMonitor(21, 2, true),
      envMonitor(22, null, false),
    ]
  )

  it('labels previews and project-level monitors', () => {
    expect(choices.map(choiceLabel)).toEqual([
      'production',
      'pr-12 (preview)',
      'm22 (project monitor)',
    ])
  })

  it('offers the first environment not already on the page', () => {
    expect(firstAvailableChoice(choices, new Set())?.monitor.id).toBe(20)
    expect(firstAvailableChoice(choices, new Set([20]))?.monitor.id).toBe(21)
    expect(firstAvailableChoice(choices, new Set([20, 21, 22]))).toBeUndefined()
  })
})

describe('switchComponent', () => {
  const [production, preview] = buildMonitorChoices(
    [environment(1, 'production'), environment(2, 'pr-12', true)],
    [envMonitor(30, 1, true), envMonitor(31, 2, true)]
  )
  const web = { id: 9, name: 'web' }

  it('lets a suggested name follow the new monitor and keeps the group', () => {
    const row = componentRow({ id: 7, name: 'billing' }, production, 'Cloud')
    expect(row.display_name).toBe('Billing')
    expect(switchComponent(row, web, production)).toMatchObject({
      monitor_id: 30,
      project_id: 9,
      project_name: 'web',
      display_name: 'Web',
      group_name: 'Cloud',
    })
  })

  it('keeps a public name the operator typed', () => {
    const row = {
      ...componentRow({ id: 7, name: 'billing' }, production),
      display_name: 'Payments',
    }
    expect(switchComponent(row, web, preview)).toMatchObject({
      monitor_id: 31,
      environment_name: 'pr-12',
      display_name: 'Payments',
    })
  })
})

describe('suggestPublicName', () => {
  it('title-cases separators and words', () => {
    expect(suggestPublicName('production')).toBe('Production')
    expect(suggestPublicName('billing_worker-eu')).toBe('Billing Worker Eu')
    expect(suggestPublicName('   ')).toBe('')
  })
})

describe('appearanceFormSchema', () => {
  const valid = {
    title: 'Acme status',
    description: '',
    theme: 'auto' as const,
    accent_color: '#4F46E5',
    logo_url: 'https://cdn.example.com/logo.svg',
    website_url: '',
    show_uptime_percentage: true,
    show_response_times: true,
  }

  it('accepts https URLs and hex colors', () => {
    expect(appearanceFormSchema.safeParse(valid).success).toBe(true)
  })

  it('rejects non-https URLs and malformed colors', () => {
    expect(
      appearanceFormSchema.safeParse({
        ...valid,
        logo_url: 'http://x.test/a.png',
      }).success
    ).toBe(false)
    expect(
      appearanceFormSchema.safeParse({
        ...valid,
        website_url: 'javascript:alert(1)',
      }).success
    ).toBe(false)
    expect(
      appearanceFormSchema.safeParse({ ...valid, accent_color: 'red' }).success
    ).toBe(false)
  })

  it('normalizes the accent color', () => {
    expect(normalizeAccent(' 4F46E5 ')).toBe('#4f46e5')
    expect(normalizeAccent('')).toBe('')
  })
})

describe('incidentTargets', () => {
  it('offers each project as a whole and each of its components', () => {
    expect(incidentTargets(page).map((t) => [t.value, t.label])).toEqual([
      ['project:7', 'Everything from Billing'],
      ['monitor:2', 'Billing'],
      ['project:9', 'Everything from Web'],
      ['monitor:3', 'Cloud · Staging'],
      ['monitor:4', 'Cloud · Dashboard'],
    ])
  })

  it('calls the single project of a page "All components"', () => {
    const single = {
      projects: [page.projects[0]],
      monitors: page.monitors.filter((m) => m.project_id === 7),
    }
    expect(incidentTargets(single)[0]).toEqual({
      value: 'project:7',
      label: 'All components',
      project_id: 7,
    })
  })
})

describe('customIncidentSchema / customIncidentBody', () => {
  const values = {
    public_title: ' Delayed email delivery ',
    severity: 'minor' as const,
    status: 'identified' as const,
    target: 'monitor:4',
    message: ' Emails arrive late. ',
    is_public: true,
  }

  it('requires a public headline, a message and a target', () => {
    expect(customIncidentSchema.safeParse(values).success).toBe(true)
    expect(
      customIncidentSchema.safeParse({ ...values, public_title: ' ' }).success
    ).toBe(false)
    expect(
      customIncidentSchema.safeParse({ ...values, message: '' }).success
    ).toBe(false)
    expect(
      customIncidentSchema.safeParse({ ...values, target: '' }).success
    ).toBe(false)
  })

  it("posts a component's incident to that component's project", () => {
    expect(customIncidentBody(values, page)).toMatchObject({
      project_id: 9,
      body: {
        title: 'Delayed email delivery',
        public_title: 'Delayed email delivery',
        monitor_id: 4,
        message: 'Emails arrive late.',
        is_public: true,
        status: 'identified',
      },
    })
  })

  it('posts a project-wide incident without a monitor', () => {
    const request = customIncidentBody({ ...values, target: 'project:7' }, page)
    expect(request?.project_id).toBe(7)
    expect(request?.body.monitor_id).toBeNull()
  })

  it('refuses a target that is no longer on the page', () => {
    expect(
      customIncidentBody({ ...values, target: 'monitor:99' }, page)
    ).toBeNull()
    expect(
      customIncidentBody({ ...values, target: 'project:99' }, page)
    ).toBeNull()
  })
})
