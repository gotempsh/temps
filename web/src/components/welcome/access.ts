// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * How people reach the apps on this box, chosen in the order of ADR-064: who
 * first, then the way in, then the address. Every option is one Temps already
 * designs for: a public wildcard on open ports (ADR-018, -058 `direct`), a
 * Cloudflare Tunnel connector (ADR-058 D3, ADR-107), and a private Tailscale
 * listener with your domain or the tailnet name (ADR-064 D5, ADR-065).
 *
 * Pure: the facts come from `GET /platform/host-facts`.
 */

export type Audience = 'public' | 'private'
export type WayIn = 'tunnel' | 'direct' | 'tailnet_domain' | 'tailnet_name'
export type Naming = 'wildcard' | 'per_app'

/** What Temps read about the box before asking anything. */
export interface BoxFacts {
  publicIp: string | null
  /** Whether ports 80 and 443 answer from the internet; `null` when not checked. */
  inbound: boolean | null
  lanIp: string | null
  tailscale: {
    ip: string
    machine: string | null
    tailnet: string | null
  } | null
  /**
   * The private network this box would serve private apps on (ADR-115 D8):
   * Tailscale, WireGuard, NetBird, the LAN… `null` when it is on none it
   * can use.
   */
  privateNetwork: PrivateNetwork | null
  /**
   * Why this control plane serves no private address at all, when the
   * server says so (a console running without its proxy): the private ways
   * say this instead of offering a plan its save would refuse.
   */
  privateUnavailable?: string | null
}

/** A private network of this box, as the private-ingress status lists it. */
export interface PrivateNetwork {
  /** `Tailscale` or the interface (`wg0`). */
  label: string
  address: string
  /** What the consent's `listen` is: `tailscale`, or the address. */
  listen: string
  /** Who can connect: `10.8.0.0/24`. */
  admit: string[]
  /** The network of a consent given before, which saving keeps as it is. */
  consented?: boolean
  /** When that consent was given or last changed: saving checks it is still the one. */
  consentAt?: string
}

/** The subset of `PrivateNetworkOption` the access step reads. */
export interface PrivateNetworkLike {
  interface: string
  address: string
  tailscale: boolean
  admit: string[]
  problem?: string | null
}

/** The first network this box can serve private apps on (Tailscale first, as the server orders them). */
export function privateNetworkOf(
  networks: PrivateNetworkLike[]
): PrivateNetwork | null {
  const usable = networks.find((n) => !n.problem && n.admit.length > 0)
  if (!usable) return null
  return {
    label: usable.tailscale ? 'Tailscale' : usable.interface,
    address: usable.address,
    listen: usable.tailscale ? 'tailscale' : usable.address,
    admit: usable.admit,
  }
}

/** The subset of `HostFacts` the access step reads. */
export interface HostFactsLike {
  public_ip?: string | null
  lan_ip?: string | null
  inbound: { checked: boolean; reachable?: boolean | null }
  tailscale: {
    running: boolean
    ip?: string | null
    machine?: string | null
    tailnet?: string | null
  }
}

/** The subset of the private-ingress status the access step reads. */
export interface PrivateIngressLike {
  available: boolean
  error?: string | null
  networks: PrivateNetworkLike[]
  /** The consent already given, if any. */
  consent?: PrivateConsentLike | null
}

/** The subset of the stored consent the access step reads. */
export interface PrivateConsentLike {
  listen: string
  admit?: string[]
  updated_at: string
}

/**
 * The network a consent given before serves on, with the ranges it admits:
 * the wizard keeps that consent, so its plan (the DNS record's address, who
 * may connect) is about that network, not the first one listed. No network,
 * and why, when this machine does not list the one the consent names, or
 * refuses to serve it (it overlaps a container network, Docker cannot be
 * listed): the plan would point DNS at an address nothing serves.
 */
export function consentedNetwork(
  consent: PrivateConsentLike,
  networks: PrivateNetworkLike[]
): { network: PrivateNetwork | null; problem: string | null } {
  const onTailscale = consent.listen === 'tailscale'
  const where = onTailscale ? 'Tailscale' : consent.listen
  const option = networks.find((n) =>
    onTailscale ? n.tailscale : n.address === consent.listen
  )
  if (!option) {
    return {
      network: null,
      problem: `The private-ingress consent serves on ${where}, which this machine does not list as one of its private networks now. Change the consent in Settings, then come back.`,
    }
  }
  if (option.problem) {
    return {
      network: null,
      problem: `This machine refuses to serve private addresses on ${where}, as its consent asks: ${option.problem}`,
    }
  }
  // Empty admits the network's own ranges, which its option lists.
  const admit = consent.admit?.length ? consent.admit : option.admit
  if (admit.length === 0) {
    return {
      network: null,
      problem: `The private-ingress consent on ${where} admits no range this machine can serve. Change it in Settings, then come back.`,
    }
  }
  return {
    network: {
      label: onTailscale ? 'Tailscale' : option.interface,
      address: option.address,
      listen: consent.listen,
      admit,
      consented: true,
      consentAt: consent.updated_at,
    },
    problem: null,
  }
}

/** Said when the server reports no reason of its own. */
export const PRIVATE_UNAVAILABLE =
  'This console runs without its proxy, so it serves no private addresses. Run temps serve with both in one process, or serve them from a worker.'

export function factsFromHost(
  host: HostFactsLike,
  privateIngress?: PrivateIngressLike
): BoxFacts {
  const tailscale =
    host.tailscale.running && host.tailscale.ip
      ? {
          ip: host.tailscale.ip,
          machine: host.tailscale.machine ?? null,
          tailnet: host.tailscale.tailnet ?? null,
        }
      : null
  const consented =
    privateIngress?.available && privateIngress.consent
      ? consentedNetwork(privateIngress.consent, privateIngress.networks)
      : null
  const unavailable =
    privateIngress && !privateIngress.available
      ? privateIngress.error || PRIVATE_UNAVAILABLE
      : (consented?.problem ?? null)
  return {
    publicIp: host.public_ip ?? null,
    lanIp: host.lan_ip ?? null,
    inbound: host.inbound.checked ? (host.inbound.reachable ?? null) : null,
    tailscale,
    // A console without its proxy serves nothing privately, whatever
    // networks the host is on. Otherwise the server's list; before its
    // first pass lists any (or when the status could not be read),
    // Tailscale is the one private network the host facts can name.
    privateNetwork: unavailable
      ? null
      : consented
        ? consented.network
        : privateIngress?.networks.length
          ? privateNetworkOf(privateIngress.networks)
          : tailscale && {
              label: 'Tailscale',
              address: tailscale.ip,
              listen: 'tailscale',
              admit: ['100.64.0.0/10', 'fd7a:115c:a1e0::/48'],
            },
    privateUnavailable: unavailable,
  }
}

export interface AccessChoice {
  audience: Audience
  way: WayIn
  naming: Naming
  /** With a wildcard, the base domain (apps.example.com); per app, the first app's full name. */
  domain: string
  /** A Cloudflare API token, when the way in needs one. Sent once to create the DNS provider; the console keeps no copy. */
  token: string
}

export interface WayOption {
  way: WayIn
  audience: Audience
  label: string
  detail: string
  available: boolean
  recommended: boolean
  /** What the box's facts say about this option, good or bad. */
  note?: string
}

export const AUDIENCES: Array<{ id: Audience; label: string; detail: string }> =
  [
    {
      id: 'public',
      label: 'Anyone on the internet',
      detail: 'A website, an API, something you share.',
    },
    {
      id: 'private',
      label: 'Only my devices',
      detail:
        'Over your private network: Tailscale, WireGuard, your LAN… The names set up here answer only there; apps keep their generated addresses.',
    },
  ]

const TAILSCALE_MISSING =
  'Tailscale isn’t running on this box. Install it and sign in, then check again.'

export const NO_PRIVATE_NETWORK =
  'This box is on no private network it can serve on. Join one (Tailscale, WireGuard, NetBird or your LAN), then check again.'

/**
 * The ways in this version of Temps can set up. Private ways need the
 * tailnet listener (ADR-064 D5), which is not built yet: they are shown, with
 * that said, but cannot be picked.
 */
export const SERVED_WAYS: readonly WayIn[] = [
  'tunnel',
  'direct',
  'tailnet_domain',
]
export const NOT_YET_SERVED =
  'Not in this version of Temps yet. Use your domain on your private network for now.'

function tailnetName(facts: BoxFacts): string | null {
  const ts = facts.tailscale
  return ts?.machine && ts.tailnet ? `${ts.machine}.${ts.tailnet}` : null
}

export function waysFor(audience: Audience, facts: BoxFacts): WayOption[] {
  if (audience === 'public') {
    const ip = facts.publicIp ?? 'this box’s public address'
    const directNote =
      facts.inbound === true
        ? 'Ports 80 and 443 answer from the internet.'
        : facts.inbound === false
          ? `Ports 80 and 443 don’t answer from the internet. Forward them on your router${facts.lanIp ? ` to ${facts.lanIp}` : ''} first.`
          : `Not checked from outside: open http://${ip} from a phone off this network to see if it answers.`
    return [
      {
        way: 'tunnel',
        audience,
        label: 'Cloudflare Tunnel',
        detail:
          'This box dials out to Cloudflare; no port is opened. Needs a domain on Cloudflare.',
        available: true,
        recommended: facts.inbound !== true,
        note:
          facts.inbound === true
            ? undefined
            : 'Works behind a router or firewall: nothing has to reach this box.',
      },
      {
        way: 'direct',
        audience,
        label: 'Open ports 80 and 443',
        detail: `A wildcard record to ${ip}. Any DNS provider; certificates from Let’s Encrypt.`,
        available: true,
        recommended: facts.inbound === true,
        note: directNote,
      },
    ]
  }
  const ts = facts.tailscale
  const net = facts.privateNetwork
  const name = tailnetName(facts)
  return [
    {
      way: 'tailnet_domain',
      audience,
      label: 'My domain, on my private network',
      detail: net
        ? `A wildcard record to this box’s ${net.label} address, ${net.address}. One certificate for all apps.`
        : 'A wildcard record to this box’s private address.',
      available: net !== null && SERVED_WAYS.includes('tailnet_domain'),
      recommended: net !== null && SERVED_WAYS.includes('tailnet_domain'),
      note: !net
        ? (facts.privateUnavailable ?? NO_PRIVATE_NETWORK)
        : SERVED_WAYS.includes('tailnet_domain')
          ? undefined
          : NOT_YET_SERVED,
    },
    {
      way: 'tailnet_name',
      audience,
      label: 'The tailnet name, no domain',
      detail: name
        ? `${name}, through Tailscale Serve.`
        : 'The machine’s name on the tailnet, through Tailscale Serve.',
      available:
        ts !== null && name !== null && SERVED_WAYS.includes('tailnet_name'),
      recommended: false,
      note: !ts
        ? TAILSCALE_MISSING
        : !SERVED_WAYS.includes('tailnet_name')
          ? NOT_YET_SERVED
          : name
            ? 'One app per machine name. More apps need a Tailscale Service each.'
            : 'MagicDNS didn’t answer for this box, so it has no tailnet name yet. Turn on MagicDNS in the tailnet’s DNS settings.',
    },
  ]
}

/**
 * Whether any way in can serve this audience on this box, and if not, why:
 * the first screen says it there instead of leading to a screen where nothing
 * can be picked.
 */
export function audienceStatus(
  audience: Audience,
  facts: BoxFacts
): { available: boolean; note?: string } {
  const ways = waysFor(audience, facts)
  if (ways.some((w) => w.available)) return { available: true }
  return { available: false, note: ways[0]?.note }
}

/**
 * The DNS provider a tailnet domain needs, made from the pasted token: Temps
 * adds the private record and answers the certificate's DNS challenge with it.
 */
export function dnsProviderFor(
  choice: Pick<AccessChoice, 'way' | 'domain' | 'token'>
) {
  if (!needsToken(choice.way)) return null
  const zone = zoneOf(choice.domain.trim().toLowerCase())
  return {
    name: `Cloudflare · ${zone}`,
    provider_type: 'cloudflare' as const,
    description: `Private addresses on ${zone}, added during setup`,
    credentials: {
      type: 'cloudflare' as const,
      api_token: choice.token.trim(),
      account_id: null,
    },
  }
}

/**
 * Public suffixes of two labels, under which anyone registers a domain: under
 * them the zone is the last three labels (example.co.uk), never the suffix
 * itself (co.uk). The common ones only; the token check lists the real zones.
 */
const TWO_LABEL_SUFFIXES = new Set([
  'ac.uk',
  'co.uk',
  'gov.uk',
  'ltd.uk',
  'me.uk',
  'net.uk',
  'org.uk',
  'plc.uk',
  'sch.uk',
  'com.au',
  'net.au',
  'org.au',
  'edu.au',
  'gov.au',
  'id.au',
  'asn.au',
  'co.nz',
  'net.nz',
  'org.nz',
  'ac.nz',
  'govt.nz',
  'co.jp',
  'ne.jp',
  'or.jp',
  'ac.jp',
  'go.jp',
  'co.kr',
  'or.kr',
  'co.in',
  'net.in',
  'org.in',
  'firm.in',
  'gen.in',
  'ind.in',
  'co.za',
  'org.za',
  'web.za',
  'co.il',
  'org.il',
  'co.id',
  'or.id',
  'web.id',
  'com.br',
  'net.br',
  'org.br',
  'com.mx',
  'org.mx',
  'com.ar',
  'com.co',
  'com.pe',
  'com.cn',
  'net.cn',
  'org.cn',
  'com.hk',
  'org.hk',
  'com.tw',
  'org.tw',
  'com.sg',
  'org.sg',
  'com.my',
  'com.ph',
  'com.vn',
  'co.th',
  'in.th',
  'com.tr',
  'com.ua',
  'com.pl',
  'com.es',
  'com.pt',
  'co.at',
  'or.at',
  'com.eg',
  'com.sa',
  'com.ng',
  'co.ke',
])

/** The registrable domain: the last two labels, or three under a two-label public suffix. The token check lists the real zones. */
export function zoneOf(domain: string): string {
  const labels = domain.split('.')
  const lastTwo = labels.slice(-2).join('.')
  if (labels.length > 2 && TWO_LABEL_SUFFIXES.has(lastTwo.toLowerCase())) {
    return labels.slice(-3).join('.')
  }
  return lastTwo
}

/** Whether a wildcard under this domain is deeper than Cloudflare's free certificate covers. */
export function nestedForCloudflare(domain: string): boolean {
  return domain.split('.').length > zoneOf(domain).split('.').length
}

/**
 * Cloudflare's free certificate covers one level under the zone, so a tunnel on a nested domain puts
 * apps at app-sub.example.com instead of app.sub.example.com (ADR 068 D3, `host_suffix`). Temps applies
 * it rather than asking.
 */
export function flatNames(
  choice: Pick<AccessChoice, 'way' | 'domain' | 'naming'>
): boolean {
  return (
    choice.way === 'tunnel' &&
    choice.naming === 'wildcard' &&
    nestedForCloudflare(choice.domain)
  )
}

export function flatNote(
  choice: Pick<AccessChoice, 'way' | 'domain' | 'naming'>
): string | null {
  if (!flatNames(choice)) return null
  const zone = zoneOf(choice.domain)
  return `Cloudflare’s free certificate covers one level under ${zone}, so apps will be at hello-${choice.domain.split('.')[0]}.${zone}. Use ${zone} itself for hello.${zone}.`
}

export const needsDomain = (way: WayIn) => way !== 'tailnet_name'
/**
 * Whether this flow asks for a Cloudflare token itself. A tunnel's token is
 * pasted on the tunnel setup page (`/nodes/ingress`), which also picks the
 * zone and the workers, so it is never asked for twice.
 */
export const needsToken = (way: WayIn) => way === 'tailnet_domain'

/** The two ways to name apps, said for this way in: who adds each record differs. */
export function namingsFor(
  way: WayIn
): Array<{ id: Naming; label: string; detail: string; example: string }> {
  const temps = way !== 'direct'
  return [
    {
      id: 'wildcard',
      label: 'All apps under one domain',
      detail: temps
        ? 'Temps adds one wildcard record. Every new app gets a name straight away.'
        : 'You add one wildcard record, once. Every new app gets a name straight away.',
      example: 'shop.apps.example.com, api.apps.example.com',
    },
    {
      id: 'per_app',
      label: 'A name for each app',
      detail: temps
        ? 'You pick each app’s full name, on any domain the token covers. Temps adds one record per app; nothing else in the zone changes.'
        : 'You pick each app’s full name, on any domain, and add its record when you add the app. Nothing else on the domain changes.',
      example: 'shop.com, api.example.org',
    },
  ]
}

export function domainField(choice: Pick<AccessChoice, 'way' | 'naming'>): {
  label: string
  placeholder: string
} {
  if (choice.naming === 'per_app')
    return {
      label: 'Name for your first app',
      placeholder: 'hello.example.com',
    }
  if (choice.way === 'tunnel')
    return { label: 'Domain on Cloudflare', placeholder: 'example.com' }
  return {
    label: 'Domain',
    placeholder:
      choice.way === 'direct' ? 'apps.example.com' : 'home.example.com',
  }
}

/** Null when it is a usable domain; otherwise why not, and what probably was meant. */
export function domainProblem(
  input: string
): { reason: string; suggestion?: string } | null {
  const domain = input.trim()
  if (!domain)
    return { reason: 'Enter a domain you control, such as apps.example.com.' }
  const bare = domain
    .toLowerCase()
    .replace(/^[a-z]+:\/\//, '')
    .replace(/\/.*$/, '')
    .replace(/^\*\./, '')
  if (bare !== domain)
    return {
      reason: 'Just the domain: no https://, path or wildcard.',
      suggestion: bare || undefined,
    }
  if (!/^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$/.test(domain)) {
    return {
      reason:
        'That is not a domain name. Use lowercase letters, numbers, dashes and dots, like apps.example.com.',
    }
  }
  // The server's limits (`domain_problem`): refused there, the provider
  // and consent written before it would be left behind.
  if (domain.length > 253)
    return { reason: 'A domain name is at most 253 characters.' }
  const long = domain.split('.').find((label) => label.length > 63)
  if (long)
    return {
      reason: `'${long.slice(0, 20)}…' is longer than 63 characters, the most one part of a domain name can be.`,
    }
  return null
}

/** The DNS name each way in points: the wildcard, or the one app's name. */
export function recordName(choice: AccessChoice): string {
  if (choice.naming === 'per_app') return choice.domain
  return flatNames(choice) ? `*.${zoneOf(choice.domain)}` : `*.${choice.domain}`
}

/** What is wrong with the domain or name for this way in. */
export function nameProblem(choice: AccessChoice): string | null {
  const problem = domainProblem(choice.domain)
  if (problem) {
    return choice.naming === 'per_app' && !choice.domain.trim()
      ? 'Enter the full name for your first app, such as hello.example.com.'
      : problem.reason
  }
  if (
    choice.way === 'tunnel' &&
    choice.naming === 'per_app' &&
    choice.domain.split('.').length >
      zoneOf(choice.domain).split('.').length + 1
  ) {
    const zone = zoneOf(choice.domain)
    const [first, ...rest] = choice.domain.split('.')
    const middle = rest.slice(0, rest.length - zone.split('.').length)
    return `Cloudflare’s free certificate covers one level under ${zone}. Use ${first}.${zone}, or ${first}-${middle.join('-')}.${zone}.`
  }
  return null
}

/** The Cloudflare token's permissions for each way in. Least privilege: DNS only, unless it runs a tunnel. */
export function tokenPermissions(way: WayIn): string[] {
  const dns = ['Zone · Zone · Read', 'Zone · DNS · Edit']
  return way === 'tunnel' ? [...dns, 'Account · Cloudflare Tunnel · Edit'] : dns
}

export function tokenProblem(token: string): string | null {
  const t = token.trim()
  if (!t) return 'Paste the token.'
  if (!/^[A-Za-z0-9_-]{40}$/.test(t))
    return 'Cloudflare API tokens are 40 characters: letters, numbers, - and _. Copy the whole token.'
  return null
}

/** Every way in, public first; what fits the box best leads. */
export function allWays(facts: BoxFacts): WayOption[] {
  return [...waysFor('public', facts), ...waysFor('private', facts)].sort(
    (a, b) =>
      Number(b.recommended && b.available) -
      Number(a.recommended && a.available)
  )
}

/** Everything wrong with the choice, in words. Empty when it can go ahead. */
export function accessProblems(
  choice: AccessChoice,
  facts: BoxFacts
): string[] {
  const problems: string[] = []
  const option = waysFor(choice.audience, facts).find(
    (o) => o.way === choice.way
  )
  if (!option) return ['Pick how traffic gets in.']
  if (!option.available)
    problems.push(option.note ?? `${option.label} isn’t available here.`)
  if (needsDomain(choice.way)) {
    const problem = nameProblem(choice)
    if (problem) problems.push(problem)
  }
  if (needsToken(choice.way)) {
    const problem = tokenProblem(choice.token)
    if (problem) problems.push(problem)
  }
  return problems
}

/** Where an app lands with this choice. */
export function appAddress(
  app: string,
  choice: AccessChoice | null,
  facts: BoxFacts
): string {
  const lan = (facts.lanIp ?? '127.0.0.1').replace(/\./g, '-')
  if (!choice) return `${app}.${lan}.sslip.io`
  if (choice.way === 'tailnet_name')
    return tailnetName(facts) ?? 'the tailnet name'
  if (choice.naming === 'per_app') return choice.domain
  if (flatNames(choice))
    return `${app}-${choice.domain.split('.')[0]}.${zoneOf(choice.domain)}`
  return `${app}.${choice.domain}`
}

/** Who can open an app, in one line. */
export function whoCanReach(
  choice: AccessChoice | null,
  facts: BoxFacts
): string {
  if (!choice)
    return `Only devices on this network${facts.lanIp ? ` (${facts.lanIp})` : ''}`
  if (choice.audience === 'public') return 'Anyone on the internet'
  if (choice.way === 'tailnet_domain' && facts.privateNetwork)
    return `Devices in ${facts.privateNetwork.admit.join(', ')} that can reach this box on port 443`
  return `Devices on your tailnet that its access rules let reach this box on port 443`
}

export interface DnsRecord {
  type: string
  name: string
  value: string
  /** Who adds it: you at your DNS provider, or Temps through the token. */
  by: 'you' | 'temps'
}

export interface AccessPlan {
  who: string
  example: string
  records: DnsRecord[]
  you: string[]
  temps: string[]
  /** What it costs or exposes, said up front. */
  caveats: string[]
}

/**
 * What a private way in leaves as it was. It serves the names it sets up on
 * the private network only; every app still gets its generated addresses,
 * one per environment and one per deployment, which this step does not
 * change. They answer wherever this box answers from the internet.
 */
export function generatedAddressesCaveat(facts: BoxFacts): string {
  const where =
    facts.inbound === false
      ? 'This box doesn’t answer from the internet now, so nothing reaches them from there until it does.'
      : 'They are public wherever this box answers from the internet.'
  return `Every app still gets its generated addresses, one for each environment and one for each deployment, and a private name doesn’t replace them. ${where}`
}

export function accessPlan(choice: AccessChoice, facts: BoxFacts): AccessPlan {
  const name = recordName(choice)
  const perApp = choice.naming === 'per_app'
  const zone = zoneOf(choice.domain)
  const base = {
    who: whoCanReach(choice, facts),
    example: appAddress('hello', choice, facts),
  }
  const eachNew = perApp
    ? [
        'Each new app asks for its own name when you add it, and gets its own record.',
      ]
    : []
  if (choice.way === 'direct') {
    return {
      ...base,
      records: [
        {
          // An IPv6 address is published in an AAAA record; an A record
          // holds IPv4 only, and DNS providers refuse anything else.
          type: facts.publicIp?.includes(':') ? 'AAAA' : 'A',
          name,
          value: facts.publicIp ?? 'this box’s public IP',
          by: 'you',
        },
      ],
      you: [
        perApp
          ? 'Add the record below at your DNS provider, and one like it for each new app'
          : 'Add the record below at your DNS provider',
        ...(facts.inbound === true
          ? []
          : [
              `Make sure ports 80 and 443 reach this box${facts.lanIp ? ` (forward them on your router to ${facts.lanIp})` : ''}`,
            ]),
      ],
      temps: [
        perApp
          ? `Serve ${name} on ports 80 and 443`
          : `Serve every app under ${name} on ports 80 and 443`,
        'Get each app a certificate from Let’s Encrypt on its first visit',
      ],
      caveats: [
        ...eachNew,
        'Each app’s name appears in public certificate logs.',
      ],
    }
  }
  if (choice.way === 'tunnel') {
    return {
      ...base,
      records: [
        {
          type: 'CNAME',
          name,
          value: '<tunnel id>.cfargotunnel.com',
          by: 'temps',
        },
      ],
      you: [
        `Create a Cloudflare API token limited to ${perApp ? 'the zones your apps’ names are in' : zone}, with the permissions below`,
        'Paste it on the tunnel page, and pick the machines that run it',
      ],
      temps: [
        'Create a tunnel in your Cloudflare account and run its connector on this box',
        perApp
          ? `Point ${name} at the tunnel, and each new app’s name as you add it`
          : `Point ${name} at the tunnel`,
        'Serve HTTPS only; Cloudflare holds the certificate',
      ],
      caveats: [
        ...eachNew,
        'The tunnel permission covers every tunnel in that Cloudflare account. If others live there, use a separate account for Temps.',
        'Visitors’ traffic passes through Cloudflare.',
        'This console is never put behind the tunnel; it stays on your network and tailnet.',
      ],
    }
  }
  if (choice.way === 'tailnet_domain') {
    return {
      ...base,
      records: [
        {
          type: facts.privateNetwork?.address.includes(':') ? 'AAAA' : 'A',
          name,
          value: facts.privateNetwork?.address ?? '',
          by: 'temps',
        },
      ],
      you: [
        `Create a Cloudflare API token limited to ${perApp ? 'the zones your apps’ names are in' : zone}, with the permissions below`,
        'Paste it here',
      ],
      temps: [
        `Serve the apps under ${name} only to connections on ${facts.privateNetwork?.label} (${facts.privateNetwork?.address}:443), never on a public address${facts.privateNetwork?.consented ? ', as the private-ingress consent already given says' : ''}`,
        perApp
          ? `Get ${name} a certificate by DNS challenge, and one for each new app`
          : `Get one certificate for ${name} by DNS challenge, since the private address can’t be reached from the internet`,
        `Refuse any request from outside ${facts.privateNetwork?.admit.join(', ')}`,
      ],
      caveats: [
        // Nothing moves a new app here by itself, and its generated
        // addresses keep serving it beside the private name.
        `To reach an app on this network, add ${perApp ? 'its private name' : `its name under ${name}`} on the app’s Domains page.`,
        generatedAddressesCaveat(facts),
        ...eachNew,
        `The record${perApp ? 's are' : ' is'} public but point${perApp ? '' : 's'} at a private address: only devices on that network can connect.`,
      ],
    }
  }
  return {
    ...base,
    records: [],
    you: [
      'Turn on HTTPS certificates in your tailnet’s DNS settings (it needs MagicDNS)',
    ],
    temps: [`Serve the app at ${base.example} through Tailscale Serve`],
    caveats: [
      'One app per machine name. Each further app needs a Tailscale Service: a tagged machine and an OAuth client.',
      generatedAddressesCaveat(facts),
      'The machine’s name appears in public certificate logs.',
    ],
  }
}

/** The step's one-line summary for the setup list. */
export function accessSummary(
  choice: AccessChoice | null,
  facts: BoxFacts
): string {
  if (!choice) return 'Later · this network only'
  const way =
    waysFor(choice.audience, facts).find((o) => o.way === choice.way)?.label ??
    choice.way
  if (choice.way === 'tailnet_name')
    return `Private · ${appAddress('', choice, facts)}`
  return `${choice.audience === 'public' ? 'Public' : 'Private'} · ${way} · ${choice.naming === 'per_app' ? 'a name per app' : appAddress('*', choice, facts)}`
}

/** Where the console itself is reached. A tunnel never carries it (ADR-107), so it stays on the network or tailnet. */
export function consoleOrigin(
  choice: AccessChoice | null,
  facts: BoxFacts,
  current: string
): string {
  if (
    choice &&
    choice.naming === 'wildcard' &&
    (choice.way === 'direct' || choice.way === 'tailnet_domain')
  )
    return `https://temps.${choice.domain}`
  const name = tailnetName(facts)
  if (name && choice?.way !== 'tailnet_name') return `https://${name}`
  return current
}

/** Starts on what fits the box best, so the common case is one click. */
export function initialChoice(facts: BoxFacts): AccessChoice {
  const ways = allWays(facts)
  const best = ways.find((w) => w.recommended && w.available) ?? ways[0]
  return {
    audience: best.audience,
    way: best.way,
    naming: best.way === 'tailnet_name' ? 'per_app' : 'wildcard',
    domain: '',
    token: '',
  }
}

/** One question per screen: who, how, then naming and domain if needed, then the token if needed, then the plan. */
export type AccessStage =
  'who' | 'way' | 'naming' | 'domain' | 'token' | 'confirm'

export function stagesFor(way: WayIn): AccessStage[] {
  return [
    'who',
    'way',
    ...(needsDomain(way) ? (['naming', 'domain'] as const) : []),
    ...(needsToken(way) ? (['token'] as const) : []),
    'confirm',
  ]
}

/** What stops this screen from moving on; only its own question counts. */
export function stageProblems(
  choice: AccessChoice,
  stage: AccessStage,
  facts: BoxFacts
): string[] {
  if (stage === 'naming') return []
  if (stage === 'who') {
    const status = audienceStatus(choice.audience, facts)
    return status.available ? [] : [status.note ?? 'Not available on this box.']
  }
  if (stage === 'way') {
    const option = allWays(facts).find((w) => w.way === choice.way)
    return option && !option.available
      ? [option.note ?? `${option.label} isn’t available here.`]
      : []
  }
  if (stage === 'domain') {
    const problem = nameProblem(choice)
    return problem ? [problem] : []
  }
  if (stage === 'token') {
    const problem = tokenProblem(choice.token)
    return problem ? [problem] : []
  }
  return accessProblems(choice, facts)
}

/** The `default_reach` record the server stores for this choice. */
export function defaultReachFor(choice: AccessChoice): {
  audience: Audience
  way: WayIn
  naming: Naming
  domain: string | null
} {
  const naming = choice.way === 'tailnet_name' ? 'per_app' : choice.naming
  return {
    audience: choice.audience,
    way: choice.way,
    naming,
    domain:
      needsDomain(choice.way) && naming === 'wildcard'
        ? choice.domain.trim().toLowerCase()
        : null,
  }
}
