// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  accessPlan,
  accessProblems,
  accessSummary,
  allWays,
  appAddress,
  audienceStatus,
  consoleOrigin,
  defaultReachFor,
  factsFromHost,
  flatNote,
  initialChoice,
  nameProblem,
  NOT_YET_SERVED,
  SERVED_WAYS,
  recordName,
  stageProblems,
  stagesFor,
  tokenPermissions,
  waysFor,
  type AccessChoice,
  type BoxFacts,
  dnsProviderFor,
  NO_PRIVATE_NETWORK,
  privateNetworkOf,
} from './access'

const TOKEN = 'a'.repeat(20) + 'B_c-'.repeat(5)
const FACTS: BoxFacts = {
  publicIp: '203.0.113.7',
  inbound: null,
  lanIp: '192.168.1.20',
  tailscale: {
    ip: '100.101.102.103',
    machine: 'homelab-01',
    tailnet: 'tail4b2c.ts.net',
  },
  privateNetwork: {
    label: 'Tailscale',
    address: '100.101.102.103',
    listen: 'tailscale',
    admit: ['100.64.0.0/10', 'fd7a:115c:a1e0::/48'],
  },
}
const open: BoxFacts = { ...FACTS, inbound: true }
const closed: BoxFacts = { ...FACTS, inbound: false }
const noTailscale: BoxFacts = {
  ...FACTS,
  tailscale: null,
  privateNetwork: null,
}
const wireguardOnly: BoxFacts = {
  ...FACTS,
  tailscale: null,
  privateNetwork: {
    label: 'wg0',
    address: '10.8.0.1',
    listen: '10.8.0.1',
    admit: ['10.8.0.0/24'],
  },
}
const choice = (over: Partial<AccessChoice>): AccessChoice => ({
  audience: 'public',
  way: 'tunnel',
  naming: 'wildcard',
  domain: 'example.com',
  token: TOKEN,
  ...over,
})

describe('privateNetworkOf', () => {
  test('takes the first network it can serve on, by listen address or Tailscale', () => {
    const bridged = {
      interface: 'eth1',
      address: '172.18.0.5',
      tailscale: false,
      admit: [],
      problem: 'includes the container network app-net',
    }
    const wg = {
      interface: 'wg0',
      address: '10.8.0.1',
      tailscale: false,
      admit: ['10.8.0.0/24'],
      problem: null,
    }
    expect(privateNetworkOf([bridged, wg])).toEqual({
      label: 'wg0',
      address: '10.8.0.1',
      listen: '10.8.0.1',
      admit: ['10.8.0.0/24'],
    })
    expect(
      privateNetworkOf([
        {
          ...wg,
          interface: 'tailscale0',
          address: '100.101.102.103',
          tailscale: true,
        },
      ])?.listen
    ).toBe('tailscale')
    expect(privateNetworkOf([bridged])).toBeNull()
  })
})

describe('factsFromHost', () => {
  test('an unchecked inbound probe stays unknown, and Tailscale needs an address', () => {
    const facts = factsFromHost({
      public_ip: '203.0.113.7',
      lan_ip: null,
      inbound: { checked: false, reachable: null },
      tailscale: { running: true, ip: null },
    })
    expect(facts.inbound).toBeNull()
    expect(facts.tailscale).toBeNull()
    expect(
      factsFromHost({
        inbound: { checked: true, reachable: true },
        tailscale: { running: false },
      }).inbound
    ).toBe(true)
  })
})

describe('access', () => {
  test('the facts pick the recommendation: not known open, the tunnel; open ports, the wildcard', () => {
    expect(initialChoice(FACTS).way).toBe('tunnel')
    expect(
      waysFor('public', closed).find((w) => w.way === 'direct')?.note
    ).toMatch(/don’t answer from the internet/)
    expect(
      waysFor('public', FACTS).find((w) => w.way === 'direct')?.note
    ).toMatch(/phone off this network/)
    expect(initialChoice(open).way).toBe('direct')
  })

  test('private ways need a private network and say how to get one; with it, the domain on it is served', () => {
    const [domainWay, nameWay] = waysFor('private', noTailscale)
    expect(domainWay.available).toBe(false)
    expect(domainWay.note).toBe(NO_PRIVATE_NETWORK)
    expect(nameWay.available).toBe(false)
    expect(nameWay.note).toMatch(/Install it/)
    expect(
      accessProblems(
        choice({ audience: 'private', way: 'tailnet_domain' }),
        noTailscale
      ).join(' ')
    ).toMatch(/no private network/)
    const ways = waysFor('private', FACTS)
    const domain = ways.find((w) => w.way === 'tailnet_domain')
    expect(domain?.available).toBe(true)
    expect(domain?.recommended).toBe(true)
    expect(domain?.note).toBeUndefined()
    const name = ways.find((w) => w.way === 'tailnet_name')
    expect(name?.available).toBe(false)
    expect(name?.note).toBe(NOT_YET_SERVED)
    expect(SERVED_WAYS).toEqual(['tunnel', 'direct', 'tailnet_domain'])
  })

  test('the first screen says when only-my-devices is not possible, and why', () => {
    expect(audienceStatus('public', FACTS)).toEqual({ available: true })
    expect(audienceStatus('private', FACTS)).toEqual({ available: true })
    expect(audienceStatus('private', noTailscale)).toMatchObject({
      available: false,
    })
    expect(audienceStatus('private', noTailscale).note).toBe(NO_PRIVATE_NETWORK)
    expect(
      stageProblems(
        choice({ audience: 'private', way: 'tailnet_domain' }),
        'who',
        noTailscale
      )[0]
    ).toBe(NO_PRIVATE_NETWORK)
    expect(
      stageProblems(
        choice({ audience: 'private', way: 'tailnet_domain' }),
        'who',
        FACTS
      )
    ).toEqual([])
    expect(stageProblems(choice({}), 'who', FACTS)).toEqual([])
    expect(initialChoice(FACTS).audience).toBe('public')
  })

  test('a tunnel under a nested domain uses flat names and says why', () => {
    const nested = choice({ domain: 'apps.example.com' })
    expect(accessProblems(nested, FACTS)).toEqual([])
    expect(appAddress('hello', nested, FACTS)).toBe('hello-apps.example.com')
    expect(accessPlan(nested, FACTS).records[0].name).toBe('*.example.com')
    expect(flatNote(nested)).toMatch(/one level under example\.com/)
    expect(flatNote(choice({}))).toBeNull()
    expect(
      appAddress(
        'hello',
        choice({ way: 'direct', domain: 'apps.example.com' }),
        FACTS
      )
    ).toBe('hello.apps.example.com')
  })

  test('one question per screen, each checking only its own answer', () => {
    // The tunnel's token is pasted on the tunnel page, never twice.
    expect(stagesFor('tunnel')).toEqual([
      'who',
      'way',
      'naming',
      'domain',
      'confirm',
    ])
    expect(stagesFor('direct')).toEqual([
      'who',
      'way',
      'naming',
      'domain',
      'confirm',
    ])
    expect(stagesFor('tailnet_domain')).toEqual([
      'who',
      'way',
      'naming',
      'domain',
      'token',
      'confirm',
    ])
    expect(stagesFor('tailnet_name')).toEqual(['who', 'way', 'confirm'])
    const blank = choice({ domain: '', token: '' })
    expect(stageProblems(blank, 'who', FACTS)).toEqual([])
    expect(stageProblems(blank, 'way', FACTS)).toEqual([])
    expect(stageProblems(blank, 'domain', FACTS)).toHaveLength(1)
    expect(stageProblems(choice({ token: '' }), 'domain', FACTS)).toEqual([])
    expect(stageProblems(choice({ token: '' }), 'token', FACTS)[0]).toMatch(
      /Paste the token/
    )
    expect(
      stageProblems(
        choice({ audience: 'private', way: 'tailnet_name' }),
        'way',
        noTailscale
      )[0]
    ).toMatch(/Tailscale isn’t running/)
  })

  test('all ways are one list, with what fits the box first', () => {
    expect(allWays(FACTS)[0].way).toBe('tunnel')
    expect(allWays(FACTS)).toHaveLength(4)
    expect(allWays(open)[0].way).toBe('direct')
  })

  test('tokens: checked for shape, asked for only where needed, least privilege', () => {
    expect(
      stageProblems(choice({ token: 'short' }), 'token', FACTS).join(' ')
    ).toMatch(/40 characters/)
    expect(accessProblems(choice({ token: '' }), FACTS)).toEqual([])
    expect(accessProblems(choice({ way: 'direct', token: '' }), FACTS)).toEqual(
      []
    )
    expect(tokenPermissions('tailnet_domain')).toHaveLength(2)
    expect(tokenPermissions('tunnel')).toContain(
      'Account · Cloudflare Tunnel · Edit'
    )
  })

  test('plans say who, what you do, what Temps does, and what it exposes', () => {
    const tunnel = accessPlan(choice({}), FACTS)
    expect(tunnel.who).toBe('Anyone on the internet')
    expect(tunnel.records[0].by).toBe('temps')
    expect(
      tunnel.caveats.some((c) =>
        c.includes('every tunnel in that Cloudflare account')
      )
    ).toBe(true)
    const direct = accessPlan(
      choice({ way: 'direct', domain: 'apps.example.com' }),
      closed
    )
    expect(direct.records[0]).toEqual({
      type: 'A',
      name: '*.apps.example.com',
      value: '203.0.113.7',
      by: 'you',
    })
    expect(
      direct.you.some((y) => y.includes('ports 80 and 443 reach this box'))
    ).toBe(true)
    expect(
      accessPlan(choice({ way: 'direct' }), open).you.some((y) =>
        y.includes('ports 80')
      )
    ).toBe(false)
    const tailnet = accessPlan(
      choice({
        audience: 'private',
        way: 'tailnet_domain',
        domain: 'home.example.com',
      }),
      FACTS
    )
    expect(tailnet.records[0].value).toBe('100.101.102.103')
    expect(tailnet.who).toMatch(/100\.64\.0\.0\/10/)
    expect(tailnet.temps.some((t) => t.includes('DNS challenge'))).toBe(true)
  })

  test('any private network serves your domain privately, not only Tailscale', () => {
    const domainWay = waysFor('private', wireguardOnly).find(
      (w) => w.way === 'tailnet_domain'
    )
    expect(domainWay?.available).toBe(true)
    expect(domainWay?.detail).toContain('wg0 address, 10.8.0.1')
    // The tailnet name stays Tailscale's.
    expect(
      waysFor('private', wireguardOnly).find((w) => w.way === 'tailnet_name')
        ?.available
    ).toBe(false)
    const plan = accessPlan(
      choice({
        audience: 'private',
        way: 'tailnet_domain',
        domain: 'home.example.com',
      }),
      wireguardOnly
    )
    expect(plan.records[0]).toMatchObject({ type: 'A', value: '10.8.0.1' })
    expect(plan.who).toContain('10.8.0.0/24')
    expect(plan.temps).toContain('Refuse any request from outside 10.8.0.0/24')
  })

  test('addresses, summaries and the console origin follow the choice', () => {
    expect(appAddress('hello', choice({}), FACTS)).toBe('hello.example.com')
    expect(
      appAddress(
        'hello',
        choice({ audience: 'private', way: 'tailnet_name' }),
        FACTS
      )
    ).toBe('homelab-01.tail4b2c.ts.net')
    expect(appAddress('hello', null, FACTS)).toBe('hello.192-168-1-20.sslip.io')
    expect(accessSummary(choice({}), FACTS)).toBe(
      'Public · Cloudflare Tunnel · *.example.com'
    )
    expect(accessSummary(null, FACTS)).toBe('Later · this network only')
    // A tunnel never carries the console.
    expect(consoleOrigin(choice({}), FACTS, 'http://localhost')).toBe(
      'https://homelab-01.tail4b2c.ts.net'
    )
    expect(
      consoleOrigin(
        choice({ way: 'direct', domain: 'apps.example.com' }),
        FACTS,
        'x'
      )
    ).toBe('https://temps.apps.example.com')
    expect(consoleOrigin(choice({}), noTailscale, 'http://localhost')).toBe(
      'http://localhost'
    )
  })

  test('a name per app points one record at one app', () => {
    const direct = choice({
      way: 'direct',
      naming: 'per_app',
      domain: 'shop.example.org',
    })
    expect(recordName(direct)).toBe('shop.example.org')
    expect(appAddress('landing', direct, FACTS)).toBe('shop.example.org')
    expect(accessPlan(direct, FACTS).caveats.join(' ')).toMatch(
      /Each new app asks for its own name/
    )
    expect(accessSummary(direct, FACTS)).toBe(
      'Public · Open ports 80 and 443 · a name per app'
    )
    expect(
      flatNote(choice({ naming: 'per_app', domain: 'hello.example.com' }))
    ).toBeNull()
    expect(
      nameProblem(
        choice({ naming: 'per_app', domain: 'hello.apps.example.com' })
      )
    ).toMatch(/Use hello\.example\.com, or hello-apps\.example\.com/)
    expect(
      nameProblem(
        choice({
          way: 'direct',
          naming: 'per_app',
          domain: 'hello.apps.example.com',
        })
      )
    ).toBeNull()
    expect(nameProblem(choice({ naming: 'per_app', domain: '' }))).toMatch(
      /full name for your first app/
    )
  })

  test('the stored default reach matches what the server validates', () => {
    expect(defaultReachFor(choice({ domain: ' Example.COM ' }))).toEqual({
      audience: 'public',
      way: 'tunnel',
      naming: 'wildcard',
      domain: 'example.com',
    })
    expect(
      defaultReachFor(
        choice({ audience: 'private', way: 'tailnet_name', naming: 'wildcard' })
      )
    ).toEqual({
      audience: 'private',
      way: 'tailnet_name',
      naming: 'per_app',
      domain: null,
    })
    expect(
      defaultReachFor(
        choice({ naming: 'per_app', domain: 'hello.example.com' })
      ).domain
    ).toBeNull()
  })
})

describe('dnsProviderFor', () => {
  test('a tailnet domain makes a Cloudflare provider from the pasted token', () => {
    const provider = dnsProviderFor({
      way: 'tailnet_domain',
      domain: ' Home.Example.com ',
      token: ' abc_DEF-123 ',
    })
    expect(provider).toEqual({
      name: 'Cloudflare · example.com',
      provider_type: 'cloudflare',
      description: 'Private addresses on example.com, added during setup',
      credentials: {
        type: 'cloudflare',
        api_token: 'abc_DEF-123',
        account_id: null,
      },
    })
  })

  test('other ways in make no provider', () => {
    for (const way of ['tunnel', 'direct', 'tailnet_name'] as const) {
      expect(
        dnsProviderFor({ way, domain: 'apps.example.com', token: 'x' })
      ).toBeNull()
    }
  })
})
