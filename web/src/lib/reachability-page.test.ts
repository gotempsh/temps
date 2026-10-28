// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AudienceChoice, RunView } from '@/api/client'
import {
  addressesLeftAlone,
  audienceCards,
  dnsHandling,
  reachabilityPath,
  readPageParams,
  resolveEnvironmentParam,
  runPollInterval,
  selectAudience,
  unfinishedRun,
  writePageParams,
} from './reachability-page'

const controlPlane = {
  reason:
    'production runs on the control plane, and a tunnel only carries what workers serve.',
  action: null,
}

function choice(overrides: Partial<AudienceChoice>): AudienceChoice {
  return {
    audience: 'public',
    gate: null,
    available: true,
    blocked: null,
    suggested_address: null,
    ways_in: [],
    not_offered: [],
    ...overrides,
  }
}

const choices: AudienceChoice[] = [
  choice({ audience: 'public' }),
  choice({ audience: 'signed_in', gate: 'temps' }),
  choice({
    audience: 'signed_in',
    gate: 'cloudflare_access',
    available: false,
    blocked: { reason: 'Cloudflare Access works only through a tunnel.' },
  }),
  choice({
    audience: 'private',
    available: false,
    blocked: { reason: 'No worker listens on a private address.' },
  }),
  choice({
    audience: 'tailnet_users',
    available: false,
    blocked: { reason: 'No agent can tell who a tailnet visitor is.' },
  }),
]

describe('the URL', () => {
  test('reads what it knows and ignores the rest', () => {
    const params = readPageParams(
      new URLSearchParams(
        'audience=signed_in&gate=cloudflare_access&address=%20app.example.com%20&way=new_tunnel&run=12&tab=x'
      )
    )
    expect(params).toEqual({
      audience: 'signed_in',
      gate: 'cloudflare_access',
      address: 'app.example.com',
      way: 'new_tunnel',
      run: 12,
    })
    expect(
      readPageParams(
        new URLSearchParams('audience=everyone&gate=temps&way=magic&run=-3')
      )
    ).toEqual({ audience: null, gate: null, address: '', way: null, run: null })
  })

  test('a gate only travels with signed_in', () => {
    expect(
      readPageParams(new URLSearchParams('audience=public&gate=temps')).gate
    ).toBeNull()
    const next = writePageParams(
      new URLSearchParams('audience=signed_in&gate=cloudflare_access'),
      { audience: 'private' }
    )
    expect(next.toString()).toBe('audience=private')
  })

  test('writing keeps parameters the page does not own and drops empty ones', () => {
    const next = writePageParams(
      new URLSearchParams('environment=4&address=old.example.com&run=3'),
      { address: '  ', run: null, audience: 'public' }
    )
    expect(next.get('environment')).toBe('4')
    expect(next.has('address')).toBe(false)
    expect(next.has('run')).toBe(false)
    expect(next.get('audience')).toBe('public')
  })

  test('every entry point builds the same link', () => {
    expect(reachabilityPath('shop', 7)).toBe(
      '/projects/shop/environments/7/reachability'
    )
    expect(
      reachabilityPath('shop', 7, {
        address: 'shop-production.example.com',
        way: 'tunnel_environment_hosts',
      })
    ).toBe(
      '/projects/shop/environments/7/reachability?address=shop-production.example.com&way=tunnel_environment_hosts'
    )
    expect(reachabilityPath('shop', 7, { run: 31 })).toBe(
      '/projects/shop/environments/7/reachability?run=31'
    )
  })

  test('names an environment by id, or by name or slug when typed', () => {
    const environments = [
      { id: 4, name: 'production', slug: 'production' },
      { id: 9, name: 'Staging', slug: 'staging-2' },
    ]
    expect(resolveEnvironmentParam('9', environments)?.id).toBe(9)
    expect(resolveEnvironmentParam('staging', environments)?.id).toBe(9)
    expect(resolveEnvironmentParam('STAGING-2', environments)?.id).toBe(9)
    expect(resolveEnvironmentParam('42', environments)).toBeUndefined()
    expect(resolveEnvironmentParam(undefined, environments)).toBeUndefined()
  })
})

describe('what the user can choose', () => {
  test('one card per audience, signed_in available when either gate is', () => {
    const cards = audienceCards(choices)
    expect(cards.map((c) => [c.audience, c.available])).toEqual([
      ['public', true],
      ['signed_in', true],
      ['private', false],
      ['tailnet_users', false],
    ])
    expect(cards[1].entries).toHaveLength(2)
    expect(cards[1].blocked).toBeNull()
    expect(cards[2].blocked?.reason).toBe(
      'No worker listens on a private address.'
    )
  })

  test('the audience the URL asks for, when it can work', () => {
    const selection = selectAudience(audienceCards(choices), {
      audience: 'signed_in',
      gate: null,
    })
    expect(selection.audience).toBe('signed_in')
    expect(selection.gate).toBe('temps')
    expect(selection.refused).toBeNull()
  })

  test('a gate that cannot work falls back to one that can', () => {
    const selection = selectAudience(audienceCards(choices), {
      audience: 'signed_in',
      gate: 'cloudflare_access',
    })
    expect(selection.gate).toBe('temps')
    expect(selection.choice?.gate).toBe('temps')
  })

  test('an audience that cannot work is refused with its reason, not silently swapped', () => {
    const selection = selectAudience(audienceCards(choices), {
      audience: 'private',
      gate: null,
    })
    expect(selection.audience).toBe('public')
    expect(selection.refused?.audience).toBe('private')
    expect(selection.refused?.blocked?.reason).toBe(
      'No worker listens on a private address.'
    )
  })

  test('nothing is chosen when nothing can work', () => {
    const none = choices.map((c) => ({
      ...c,
      available: false,
      blocked: controlPlane,
    }))
    const selection = selectAudience(audienceCards(none), {
      audience: null,
      gate: null,
    })
    expect(selection).toEqual({
      audience: null,
      gate: null,
      choice: null,
      refused: null,
    })
  })
})

describe('runs', () => {
  const run = (id: number, state: RunView['state']) =>
    ({ id, state }) as RunView

  test('points back to the newest run while it is not over', () => {
    expect(unfinishedRun([run(5, 'waiting'), run(4, 'succeeded')])?.id).toBe(5)
    expect(unfinishedRun([run(6, 'undo_paused')])?.id).toBe(6)
    expect(unfinishedRun([run(7, 'conflict')])?.id).toBe(7)
  })

  test('is watched closely while it acts, slowly while it waits, not once over', () => {
    expect(runPollInterval(undefined)).toBe(2_000)
    expect(runPollInterval('running')).toBe(2_000)
    expect(runPollInterval('recovering')).toBe(2_000)
    expect(runPollInterval('waiting')).toBe(15_000)
    expect(runPollInterval('succeeded')).toBe(false)
    expect(runPollInterval('conflict')).toBe(false)
  })

  test('a finished newest run, or none, needs no pointer', () => {
    expect(unfinishedRun([run(5, 'succeeded'), run(4, 'waiting')])).toBe(
      undefined
    )
    expect(unfinishedRun([run(5, 'undone')])).toBe(undefined)
    expect(unfinishedRun([])).toBe(undefined)
    expect(unfinishedRun(undefined)).toBe(undefined)
  })
})

describe('the plan in words', () => {
  const step = (kind: string) => ({ kind, label: kind }) as never
  test('says who handles DNS', () => {
    expect(
      dnsHandling({
        kind: 'direct_manual_dns',
        way_in: 'direct:edge-1',
        steps: [step('attach_domain'), step('show_dns_record')],
      })
    ).toContain('You add one DNS record')
    expect(
      dnsHandling({
        kind: 'direct_managed_dns',
        way_in: 'direct:edge-1',
        steps: [step('set_dns_record')],
      })
    ).toContain('Temps writes the DNS record')
    expect(
      dnsHandling({
        kind: 'existing_tunnel',
        way_in: 'tunnel:homelab',
        steps: [step('attach_domain'), step('wait_for_publication')],
      })
    ).toContain('tunnel publishes')
    expect(
      dnsHandling({
        kind: 'tailscale_serve',
        way_in: 'private:homelab',
        steps: [step('configure_tailscale_serve')],
      })
    ).toContain('inside your tailnet')
  })

  test('a step kind the console has never seen changes nothing', () => {
    expect(
      dnsHandling({
        kind: 'existing_tunnel',
        way_in: 'tunnel:homelab',
        steps: [step('enable_https_redirect')],
      })
    ).toContain('tunnel publishes')
  })

  test('lists the other addresses as left alone', () => {
    const addresses = [
      { address: 'App.example.com', audience: 'public' as const },
      { address: 'shop-production.localho.st', audience: 'public' as const },
    ]
    expect(addressesLeftAlone(addresses, 'app.example.com')).toEqual([
      addresses[1],
    ])
    expect(addressesLeftAlone(addresses, null)).toEqual(addresses)
  })
})
