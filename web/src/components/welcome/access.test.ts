// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  accessPlan,
  consentedNetwork,
  AUDIENCES,
  generatedAddressesCaveat,
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

  test('a console without its proxy offers no private plan, and says why', () => {
    const host = {
      inbound: { checked: false, reachable: null },
      tailscale: {
        running: true,
        ip: '100.101.102.103',
        machine: 'box-01',
        tailnet: 'tail0000.ts.net',
      },
    }
    const reason = 'This control plane runs its console and proxy apart.'
    const split = factsFromHost(host, {
      available: false,
      error: reason,
      networks: [],
    })
    expect(split.privateNetwork).toBeNull()
    const [domainWay] = waysFor('private', split)
    expect(domainWay.available).toBe(false)
    expect(domainWay.recommended).toBe(false)
    expect(domainWay.note).toBe(reason)
    expect(audienceStatus('private', split)).toEqual({
      available: false,
      note: reason,
    })
    expect(initialChoice(split).audience).toBe('public')
    // The same host where the console serves privately itself.
    expect(
      factsFromHost(host, { available: true, networks: [] }).privateNetwork
        ?.listen
    ).toBe('tailscale')
  })
})

describe('a consent given before', () => {
  const host = {
    inbound: { checked: false, reachable: null },
    tailscale: { running: true, ip: '100.101.102.103' },
  }
  const networks = [
    {
      interface: 'tailscale0',
      address: '100.101.102.103',
      tailscale: true,
      admit: ['100.64.0.0/10'],
    },
    {
      interface: 'eth0',
      address: '192.168.1.2',
      tailscale: false,
      admit: ['192.168.1.0/24'],
    },
  ]
  const at = '2026-11-27T10:00:00Z'

  test('plans on the network it serves, not the first one listed', () => {
    const facts = factsFromHost(host, {
      available: true,
      networks,
      consent: {
        listen: '192.168.1.2',
        admit: ['192.168.1.0/28'],
        updated_at: at,
      },
    })
    expect(facts.privateNetwork).toEqual({
      label: 'eth0',
      address: '192.168.1.2',
      listen: '192.168.1.2',
      admit: ['192.168.1.0/28'],
      consented: true,
      consentAt: at,
    })
    const plan = accessPlan(
      choice({
        audience: 'private',
        way: 'tailnet_domain',
        naming: 'wildcard',
        domain: 'apps.example.com',
      }),
      facts
    )
    expect(plan.records[0]?.value).toBe('192.168.1.2')
    expect(plan.temps.join(' ')).toContain(
      'Refuse any request from outside 192.168.1.0/28'
    )
  })

  test('with its default ranges, admits what its network does', () => {
    expect(
      consentedNetwork(
        { listen: 'tailscale', admit: [], updated_at: at },
        networks
      ).network?.admit
    ).toEqual(['100.64.0.0/10'])
  })

  test('a network this machine refuses to serve, or no longer lists, is not offered, and says why', () => {
    // It now overlaps a container network, or Docker could not be listed.
    const refused = networks.map((n) =>
      n.interface === 'eth0'
        ? {
            ...n,
            problem:
              'it overlaps the container network app-net (192.168.1.0/24)',
          }
        : n
    )
    const facts = factsFromHost(host, {
      available: true,
      networks: refused,
      consent: { listen: '192.168.1.2', updated_at: at },
    })
    expect(facts.privateNetwork).toBeNull()
    expect(facts.privateUnavailable).toContain(
      'refuses to serve private addresses on 192.168.1.2'
    )
    expect(facts.privateUnavailable).toContain('app-net')
    const [domainWay] = waysFor('private', facts)
    expect(domainWay.available).toBe(false)
    // Not the first usable network instead: the consent is kept, and is on eth0.
    const gone = consentedNetwork(
      { listen: '10.9.0.1', updated_at: at },
      networks
    )
    expect(gone.network).toBeNull()
    expect(gone.problem).toContain('does not list')
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
    // A zone under a two-label public suffix is the apex, not nested: the
    // zone is example.co.uk, never co.uk.
    const uk = choice({ domain: 'example.co.uk' })
    expect(accessProblems(uk, FACTS)).toEqual([])
    expect(flatNote(uk)).toBeNull()
    expect(appAddress('hello', uk, FACTS)).toBe('hello.example.co.uk')
    expect(accessPlan(uk, FACTS).records[0].name).toBe('*.example.co.uk')
    const ukNested = choice({ domain: 'apps.example.co.uk' })
    expect(appAddress('hello', ukNested, FACTS)).toBe(
      'hello-apps.example.co.uk'
    )
    expect(flatNote(ukNested)).toMatch(/one level under example\.co\.uk/)
    expect(
      nameProblem(
        choice({ naming: 'per_app', domain: 'hello.apps.example.co.uk' })
      )
    ).toMatch(/Use hello\.example\.co\.uk, or hello-apps\.example\.co\.uk/)
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
    // A box whose public address is IPv6 publishes it in an AAAA record.
    const v6 = accessPlan(
      choice({ way: 'direct', domain: 'apps.example.com' }),
      {
        ...closed,
        publicIp: '2001:db8::42',
      }
    )
    expect(v6.records[0]).toEqual({
      type: 'AAAA',
      name: '*.apps.example.com',
      value: '2001:db8::42',
      by: 'you',
    })
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
    // It promises privacy only for the names it sets up, and says every
    // app keeps its generated addresses, per environment and per
    // deployment, which it does not change.
    expect(
      tailnet.temps.some((t) => t.includes('apps under *.home.example.com'))
    ).toBe(true)
    expect(tailnet.temps.some((t) => /^Serve these apps/.test(t))).toBe(false)
    expect(tailnet.caveats[0]).toContain('Domains page')
    expect(tailnet.caveats[0]).not.toMatch(/alone|only/)
    expect(tailnet.caveats[1]).toMatch(
      /one for each environment and one for each deployment, and a private name doesn’t replace them/
    )
    expect(tailnet.caveats[1]).not.toMatch(/answer from the internet now/)
    expect(
      accessPlan(
        choice({
          audience: 'private',
          way: 'tailnet_domain',
          domain: 'home.example.com',
        }),
        closed
      ).caveats[1]
    ).toMatch(/doesn’t answer from the internet now/)
    // The Tailscale Serve name says the same, and the choice itself no
    // longer promises that nothing is reachable from the internet.
    expect(
      accessPlan(choice({ audience: 'private', way: 'tailnet_name' }), FACTS)
        .caveats
    ).toContain(generatedAddressesCaveat(FACTS))
    expect(AUDIENCES.find((a) => a.id === 'private')?.detail).not.toMatch(
      /Nothing is reachable/
    )
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
