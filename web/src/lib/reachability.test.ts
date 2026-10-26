// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  AddressReachability,
  EnvironmentReachability,
  JournalEntry,
  RunView,
  SetupField,
} from '@/api/client'
import {
  applyValues,
  canResumeRun,
  canUndoRun,
  conflictChoices,
  conflictSubject,
  conflictsToDecide,
  resumeNeedsTailscaleCredential,
  resumeNeedsToken,
  stalePlan,
  certificateChoices,
  defaultOption,
  missingAnswers,
  planIsCurrent,
  reachabilityHeadline,
  audienceLabel,
  privateDnsWarning,
  parsePrincipals,
  wayInLabel,
} from './reachability'

const address = (
  overrides: Partial<AddressReachability>
): AddressReachability => ({
  address: 'app.example.com',
  audience: 'public',
  source: 'custom_domain',
  state: 'not_reachable',
  reason: 'No DNS record for app.example.com exists yet.',
  ...overrides,
})

const environment = (
  overrides: Partial<EnvironmentReachability>
): EnvironmentReachability => ({
  project_id: 1,
  environment_id: 2,
  environment_name: 'production',
  deployed: true,
  running_on: ['worker-1'],
  state: 'not_reachable',
  summary: 'It runs on worker-1, which has no way in.',
  addresses: [],
  plan: null,
  ...overrides,
})

describe('reachability headline', () => {
  test('names the reachable address, a custom domain before the generated one', () => {
    const headline = reachabilityHeadline(
      environment({
        state: 'reachable',
        addresses: [
          address({
            address: 'production.example.org',
            source: 'generated',
            state: 'reachable',
          }),
          address({ address: 'shop.example.com', state: 'reachable' }),
        ],
      })
    )
    expect(headline).toEqual({
      kind: 'public',
      address: 'shop.example.com',
      url: 'https://shop.example.com',
    })
  })

  test('says why not, and offers the fix only when the plan has an option', () => {
    const withPlan = reachabilityHeadline(
      environment({
        plan: {
          audience: 'public',
          options: [
            {
              kind: 'new_tunnel_with_token',
              title: 'Through a new Cloudflare Tunnel',
              description: '',
              actions: 3,
              recommended: true,
              way_in: 'tunnel:new',
              steps: [],
              fields: [],
              warnings: [],
              required_permissions: [],
            },
          ],
        },
      })
    )
    expect(withPlan).toEqual({
      kind: 'not_reachable',
      reason: 'It runs on worker-1, which has no way in.',
      canFix: true,
    })
    const blocked = reachabilityHeadline(environment({ plan: null }))
    expect(blocked.kind === 'not_reachable' && blocked.canFix).toBe(false)
  })

  test('an address Temps cannot judge is unknown, not broken', () => {
    expect(
      reachabilityHeadline(
        environment({ state: 'unknown', summary: 'DNS points elsewhere.' })
      )
    ).toEqual({ kind: 'unknown', reason: 'DNS points elsewhere.' })
  })
})

test('ways in read as words', () => {
  expect(wayInLabel('tunnel:homelab')).toBe('Tunnel homelab')
  expect(wayInLabel('direct:edge-1')).toBe('Straight to edge-1')
  expect(wayInLabel('direct:control-plane')).toBe('This server')
  expect(wayInLabel(null)).toBe('No way in yet')
})

describe('runs', () => {
  const run = (overrides: Partial<RunView>): RunView => ({
    id: 1,
    audience: 'public',
    option: 'existing_tunnel',
    state: 'succeeded',
    steps: [],
    created: [{ type: 'custom_domain', id: 4, domain: 'app.example.com' }],
    journal: [],
    undo: [],
    created_at: '',
    updated_at: '',
    ...overrides,
  })

  test('undo is offered while something it created is left', () => {
    expect(canUndoRun(run({}))).toBe(true)
    expect(canUndoRun(run({ state: 'running' }))).toBe(false)
    expect(canUndoRun(run({ state: 'undone' }))).toBe(false)
    expect(canUndoRun(run({ created: [] }))).toBe(false)
    // Recovering after a restart: wait until Temps has checked.
    expect(canUndoRun(run({ state: 'recovering' }))).toBe(false)
    expect(
      canUndoRun(
        run({
          state: 'interrupted',
          journal: [
            {
              key: 'run-1/connector/t',
              intent: { type: 'connector', name: 't' },
              state: 'pending',
              at: '',
            },
          ],
        })
      )
    ).toBe(false)
    // What undo removed leaves the journal's done entries.
    expect(
      canUndoRun(
        run({
          state: 'failed',
          created: [],
          undo: [
            {
              resource: {
                type: 'custom_domain',
                id: 4,
                domain: 'app.example.com',
              },
              label: 'Domain app.example.com',
              ok: true,
            },
          ],
        })
      )
    ).toBe(false)
  })
})

test('waiting and interrupted runs resume, asking for the token only when needed', () => {
  const base: RunView = {
    id: 2,
    audience: 'public',
    option: 'new_tunnel_with_token',
    state: 'interrupted',
    steps: [
      { kind: 'check_token', label: 'Check', status: 'done' },
      { kind: 'save_dns_provider', label: 'Save', status: 'pending' },
    ],
    created: [],
    journal: [],
    undo: [],
    created_at: '',
    updated_at: '',
  }
  expect(canResumeRun(base)).toBe(true)
  expect(resumeNeedsToken(base)).toBe(true)
  expect(canResumeRun({ ...base, state: 'failed' })).toBe(false)
  expect(canResumeRun({ ...base, state: 'recovering' })).toBe(false)
  expect(
    resumeNeedsToken({
      ...base,
      steps: [{ kind: 'save_dns_provider', label: 'Save', status: 'done' }],
    })
  ).toBe(false)
})

test('a refused apply hands back the new plan', () => {
  const plan = { audience: 'public', options: [], plan_id: 'p2' }
  expect(stalePlan({ status: 409, detail: 'changed', plan })).toEqual(
    plan as never
  )
  expect(stalePlan({ status: 409, detail: 'busy' })).toBeNull()
  expect(stalePlan({ plan: { options: 'no' } })).toBeNull()
  expect(stalePlan(new Error('network'))).toBeNull()
})

describe('apply answers', () => {
  const fields: SetupField[] = [
    {
      key: 'address',
      label: 'Address',
      kind: { type: 'text' },
      required: true,
    },
    {
      key: 'dns_provider_id',
      label: 'Cloudflare account',
      kind: { type: 'dns_provider', provider_type: 'cloudflare' },
      required: true,
    },
    {
      key: 'cloudflare_api_token',
      label: 'API token',
      kind: { type: 'secret' },
      required: false,
    },
  ]

  test('carry provider ids as numbers and leave blanks out', () => {
    expect(
      applyValues(
        fields,
        {
          address: ' app.example.com ',
          dns_provider_id: '7',
          cloudflare_api_token: '',
        },
        null
      )
    ).toEqual({ address: 'app.example.com', dns_provider_id: 7 })
  })

  test('send the planned address when the option does not ask for one', () => {
    expect(applyValues([], {}, 'app.example.com')).toEqual({
      address: 'app.example.com',
    })
  })

  test('name what is still missing', () => {
    expect(missingAnswers(fields, { address: 'x.example.com' })).toEqual([
      'Cloudflare account',
    ])
  })

  test('the recommended option is preselected', () => {
    const option = (recommended: boolean, title: string) => ({
      kind: 'existing_tunnel' as const,
      title,
      description: '',
      actions: 1,
      recommended,
      way_in: '',
      steps: [],
      fields: [],
      warnings: [],
      required_permissions: [],
    })
    expect(defaultOption([option(false, 'a'), option(true, 'b')])?.title).toBe(
      'b'
    )
    expect(defaultOption([option(false, 'a')])?.title).toBe('a')
    expect(defaultOption([])).toBeUndefined()
  })
})

describe('certificate choices', () => {
  test('HTTP-01 is recommended only when a direct ingress takes port 80', () => {
    expect(
      certificateChoices({
        coveringTunnels: 0,
        wildcard: false,
        http01Available: true,
      })
    ).toEqual({ showHttp01: true, recommended: 'http-01', http01Note: null })
    // Port forwarding can bring port 80 to a server Temps sees listening
    // elsewhere: still listed, not recommended, and the note says why.
    const noPort80 = certificateChoices({
      coveringTunnels: 0,
      wildcard: false,
      http01Available: false,
    })
    expect(noPort80.showHttp01).toBe(true)
    expect(noPort80.recommended).toBeNull()
    expect(noPort80.http01Note).toContain('port 80')
  })

  test('while Temps cannot tell, HTTP-01 is listed but not recommended', () => {
    expect(
      certificateChoices({
        coveringTunnels: 0,
        wildcard: false,
        http01Available: undefined,
      })
    ).toEqual({ showHttp01: true, recommended: null, http01Note: null })
  })

  test('a covering tunnel wins, and wildcards need DNS-01', () => {
    expect(
      certificateChoices({
        coveringTunnels: 1,
        wildcard: false,
        http01Available: true,
      }).recommended
    ).toBe('tunnel')
    expect(
      certificateChoices({
        coveringTunnels: 0,
        wildcard: true,
        http01Available: true,
      })
    ).toEqual({ showHttp01: false, recommended: 'dns-01', http01Note: null })
  })
})

describe('conflicts (ADR 064 D4)', () => {
  const entry = (
    overrides: Partial<JournalEntry> & Pick<JournalEntry, 'intent'>
  ): JournalEntry => ({
    key: 'run-1/dns_record/app.example.com/A',
    state: 'conflict',
    at: '',
    conflict: 'A 192.0.2.44 (not set by Temps)',
    ...overrides,
  })
  const record = entry({
    intent: {
      type: 'dns_record',
      hostname: 'app.example.com',
      label: '',
      record_type: 'A',
    },
  })
  const tunnel = entry({
    key: 'run-1/connector/homelab',
    intent: { type: 'connector', name: 'homelab' },
    conflict: 'other.example.com',
  })
  const view = (state: RunView['state'], journal: JournalEntry[]): RunView => ({
    id: 1,
    audience: 'public',
    created: [],
    journal,
    created_at: '',
    updated_at: '',
    option: 'direct_managed_dns',
    state,
    steps: [],
    undo: [],
  })

  test('lists what waits for a decision, and only then', () => {
    expect(conflictsToDecide(view('conflict', [record]))).toHaveLength(1)
    expect(conflictsToDecide(view('failed', [record]))).toHaveLength(0)
    // While undoing, one Temps never wrote is not in the way.
    expect(
      conflictsToDecide(view('undo_paused', [{ ...record, unwritten: true }]))
    ).toHaveLength(0)
    expect(canUndoRun({ ...view('undo_paused', [tunnel]), created: [] })).toBe(
      false
    )
  })

  test('names the record or tunnel and offers what undo may do', () => {
    expect(conflictSubject(record)).toBe('The A record for app.example.com')
    expect(conflictSubject(tunnel)).toBe('Tunnel homelab')
    expect(conflictChoices(tunnel, true).map((c) => c.label)).toEqual([
      'Keep the tunnel',
      'Remove it anyway',
    ])
    // A record this run did not write is never deleted by undo.
    expect(conflictChoices(record, true).map((c) => c.decision)).toEqual([
      'keep',
    ])
    expect(conflictChoices(record, false).map((c) => c.decision)).toEqual([
      'keep',
      'overwrite',
    ])
  })
})

describe('private addresses (ADR 064 D5)', () => {
  test('the headline says private, with the network', () => {
    const headline = reachabilityHeadline(
      environment({
        state: 'reachable',
        addresses: [
          address({
            address: 'internal.example.com',
            audience: 'private',
            network: 'Tailscale',
            state: 'reachable',
            way_in: 'private:homelab',
          }),
        ],
      })
    )
    expect(headline).toEqual({
      kind: 'private',
      address: 'internal.example.com',
      url: 'https://internal.example.com',
      network: 'Tailscale',
    })
    expect(wayInLabel('private:homelab')).toBe('Private listener on homelab')
  })

  test('Domains names the network a private address is for', () => {
    expect(audienceLabel({ audience: 'private', network: 'Tailscale' })).toBe(
      'Private: Tailscale only'
    )
    expect(
      audienceLabel({ audience: 'private', network: '192.168.0.0/16' })
    ).toBe('Private: 192.168.0.0/16 only')
    expect(audienceLabel({ audience: 'private', network: null })).toBe(
      'Private'
    )
    expect(audienceLabel({ audience: 'public' })).toBe('Anyone')
  })

  test('the private option warns that its DNS record reveals the name', () => {
    const option = {
      kind: 'private_worker' as const,
      title: 'Only my private network, through homelab',
      description: '',
      actions: 1,
      recommended: true,
      way_in: 'private:homelab',
      steps: [],
      fields: [],
      warnings: [
        'This adds a public DNS record: anyone can look up internal.example.com and see that it exists and that it points at 100.101.102.103.',
      ],
      required_permissions: [],
    }
    expect(privateDnsWarning(option)).toContain('100.101.102.103')
    expect(
      privateDnsWarning({ ...option, kind: 'direct_managed_dns' })
    ).toBeNull()
  })
})

describe('tailnet names (ADR 065)', () => {
  test('a Serve entry in conflict on undo can only be left', () => {
    const entry = {
      at: '2026-10-25T10:00:00Z',
      key: 'run-1:tailscale_serve',
      state: 'conflict' as const,
      intent: {
        type: 'tailscale_serve' as const,
        node_id: 2,
        node_name: 'homelab',
        address_id: 'custom:4',
        hostname: 'homelab.example.ts.net',
      },
    }
    expect(conflictSubject(entry)).toBe('homelab.example.ts.net on homelab')
    expect(conflictChoices(entry, true)).toEqual([
      { decision: 'keep', label: 'Leave the worker as it is' },
    ])
  })

  test('undo asks before deleting a Tailscale Service', () => {
    const entry = {
      at: '2026-10-25T10:00:00Z',
      key: 'run-1:tailscale_service/example.ts.net/svc:myapp',
      state: 'conflict' as const,
      intent: {
        type: 'tailscale_service' as const,
        tailnet: 'example.ts.net',
        name: 'svc:myapp',
        marker: 'temps:inst1:1',
      },
    }
    expect(conflictSubject(entry)).toBe('Tailscale Service `svc:myapp`')
    expect(conflictChoices(entry, true)).toEqual([
      { decision: 'keep', label: 'Keep it' },
      { decision: 'overwrite', label: 'Delete it' },
    ])
  })

  test('resuming asks for the OAuth client again only before it is saved', () => {
    const run: RunView = {
      id: 3,
      audience: 'private',
      option: 'tailscale_service_with_credential',
      state: 'interrupted',
      steps: [
        {
          kind: 'save_tailscale_credential',
          label: 'Save',
          status: 'pending',
        },
        {
          kind: 'define_tailscale_service',
          label: 'Define',
          status: 'pending',
        },
      ],
      created: [],
      journal: [],
      undo: [],
      created_at: '',
      updated_at: '',
    }
    expect(resumeNeedsTailscaleCredential(run)).toBe(true)
    expect(resumeNeedsToken(run)).toBe(false)
    expect(
      resumeNeedsTailscaleCredential({
        ...run,
        steps: [
          { kind: 'save_tailscale_credential', label: 'Save', status: 'done' },
        ],
      })
    ).toBe(false)
  })
})

describe('plan for the typed address', () => {
  test('is current once the address settled and its own plan arrived', () => {
    expect(
      planIsCurrent({
        typed: ' app.example.com ',
        requested: 'app.example.com',
        placeholder: false,
      })
    ).toBe(true)
  })

  test('is not while the typed address is still being debounced', () => {
    expect(
      planIsCurrent({
        typed: 'shop.example.com',
        requested: 'app.example.com',
        placeholder: false,
      })
    ).toBe(false)
  })

  test('is not while the previous plan stands in as placeholder data', () => {
    expect(
      planIsCurrent({
        typed: 'shop.example.com',
        requested: 'shop.example.com',
        placeholder: true,
      })
    ).toBe(false)
  })
})

describe('tailnet users (ADR 065 D6)', () => {
  test('logins and tags are split, lower-cased, sorted and deduplicated', () => {
    expect(
      parsePrincipals(
        'Bob@Example.com, tag:CI\nalice@example.com  bob@example.com'
      )
    ).toEqual({
      principals: [
        { kind: 'login', value: 'alice@example.com' },
        { kind: 'login', value: 'bob@example.com' },
        { kind: 'tag', value: 'tag:ci' },
      ],
      invalid: [],
    })
  })

  test('what is neither a login nor a tag is reported, never guessed', () => {
    expect(
      parsePrincipals('alice, tag:, tag:-x, tag:a_b, @example.com, a@')
    ).toEqual({
      principals: [],
      invalid: ['alice', 'tag:', 'tag:-x', 'tag:a_b', '@example.com', 'a@'],
    })
  })

  test('nothing typed is an empty list', () => {
    expect(parsePrincipals('  \n ,')).toEqual({ principals: [], invalid: [] })
  })

  test('the label names who it is for', () => {
    expect(audienceLabel({ audience: 'tailnet_users', network: null })).toBe(
      'Listed tailnet users'
    )
  })
})
