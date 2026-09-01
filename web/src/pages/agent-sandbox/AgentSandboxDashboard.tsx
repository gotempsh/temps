// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowRight,
  Bot,
  CheckCircle2,
  Container,
  Globe,
  KeyRound,
  Loader2,
  Sparkles,
  XCircle,
} from 'lucide-react'

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { listSecretsOptions } from '@/api/client/@tanstack/react-query.gen'
import { usePageTitle } from '@/hooks/usePageTitle'

// ── Types from existing endpoints ───────────────────────────────────────────

interface CatalogEntry {
  id: string
  name: string
  credential_saved: boolean
  current_auth_type: string | null
}

interface CatalogResponse {
  default_provider: string
  providers: CatalogEntry[]
}

interface SandboxStatus {
  docker_available: boolean
  image_ready: boolean
  image_name: string
  error: string | null
}

interface GatewayStatus {
  present: boolean
  running: boolean
  drift: boolean
  image: string | null
  host_port: number | null
}

// ── Tone for status cards. The dashboard's job is to surface what *needs
// attention* at a glance — green = good, amber = degraded but functional,
// red = broken/needs setup. Mapping happens per card so each surface owns
// its own definition of "ok".

type Tone = 'ok' | 'warn' | 'bad' | 'pending'

function toneClass(tone: Tone): string {
  switch (tone) {
    case 'ok':
      return 'border-green-500/30'
    case 'warn':
      return 'border-amber-500/40'
    case 'bad':
      return 'border-red-500/40'
    default:
      return ''
  }
}

function ToneIcon({ tone }: { tone: Tone }) {
  if (tone === 'ok') return <CheckCircle2 className="h-4 w-4 text-green-500" />
  if (tone === 'warn')
    return <AlertTriangle className="h-4 w-4 text-amber-500" />
  if (tone === 'bad') return <XCircle className="h-4 w-4 text-red-500" />
  return <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
}

export function AgentSandboxDashboard() {
  usePageTitle('AI Workflows')
  const { data: catalog, isPending: catalogPending } =
    useQuery<CatalogResponse>({
      queryKey: ['ai-provider-catalog'],
      queryFn: async () => {
        // TODO(sdk-regen): migrate once /settings/ai-providers endpoint is
        // added to the generated SDK.
        const r = await fetch('/api/settings/ai-providers')
        if (!r.ok) throw new Error('Failed to load AI provider catalog')
        return r.json()
      },
      staleTime: 60 * 1000,
    })

  const { data: sandboxStatus, isPending: sandboxPending } =
    useQuery<SandboxStatus | null>({
      queryKey: ['sandbox-status'],
      queryFn: async () => {
        // TODO(sdk-regen): migrate once /settings/sandbox-status endpoint is
        // added to the generated SDK.
        const r = await fetch('/api/settings/sandbox-status')
        if (!r.ok) return null
        return r.json()
      },
      staleTime: 30 * 1000,
    })

  const { data: gatewayStatus, isPending: gatewayPending } =
    useQuery<GatewayStatus | null>({
      queryKey: ['preview-gateway-status'],
      queryFn: async () => {
        // TODO(sdk-regen): migrate once /preview-gateway/status endpoint is
        // added to the generated SDK.
        const r = await fetch('/api/preview-gateway/status')
        if (!r.ok) return null
        return r.json()
      },
      staleTime: 30 * 1000,
    })

  const { data: secretsData } = useQuery(listSecretsOptions())
  const secrets = secretsData?.items ?? []

  // ── Derive status for each card ────────────────────────────────────────────
  // Provider: ok if active provider has a saved credential; bad if no provider
  // has one at all; warn if active is unconfigured but a sibling is.
  const activeProvider = catalog?.providers.find(
    (p) => p.id === catalog?.default_provider,
  )
  const anyConfigured = catalog?.providers.some((p) => p.credential_saved)
  const providerTone: Tone = catalogPending
    ? 'pending'
    : !anyConfigured
      ? 'bad'
      : activeProvider?.credential_saved
        ? 'ok'
        : 'warn'
  const providerStatus = catalogPending
    ? 'Loading…'
    : !anyConfigured
      ? 'No credentials saved'
      : activeProvider?.credential_saved
        ? `${activeProvider.name} active`
        : `${activeProvider?.name ?? 'Active provider'} needs credential`

  // Sandbox is always on. ok if Docker is available; bad otherwise.
  const sandboxTone: Tone = sandboxPending
    ? 'pending'
    : sandboxStatus?.docker_available
      ? 'ok'
      : 'bad'
  const sandboxLabel = sandboxPending
    ? 'Checking…'
    : sandboxStatus?.docker_available
      ? sandboxStatus.image_ready
        ? `Ready (${sandboxStatus.image_name})`
        : 'Ready — image builds on first run'
      : 'Docker unavailable'

  // Preview gateway: ok if running and no drift; warn if drift; bad if not
  // present or stopped.
  const gatewayTone: Tone = gatewayPending
    ? 'pending'
    : !gatewayStatus
      ? 'pending'
      : !gatewayStatus.present
        ? 'bad'
        : !gatewayStatus.running
          ? 'bad'
          : gatewayStatus.drift
            ? 'warn'
            : 'ok'
  const gatewayLabel = gatewayPending
    ? 'Checking…'
    : !gatewayStatus
      ? 'Status unavailable'
      : !gatewayStatus.present
        ? 'Not deployed'
        : !gatewayStatus.running
          ? 'Stopped'
          : gatewayStatus.drift
            ? 'Image drift detected'
            : 'Running'

  // Secrets: ok regardless of count — zero secrets is valid. We just surface
  // the count so the user knows what's wired up.
  const secretsTone: Tone = 'ok'
  const secretsLabel =
    secrets.length === 0
      ? 'No secrets'
      : `${secrets.length} secret${secrets.length === 1 ? '' : 's'} configured`

  const cards = [
    {
      title: 'AI Providers',
      to: '/agent-sandbox/providers',
      icon: Sparkles,
      tone: providerTone,
      status: providerStatus,
      hint: 'Claude Code, Codex, OpenCode — each has its own credential.',
    },
    {
      title: 'Sandbox',
      to: '/agent-sandbox/sandbox',
      icon: Bot,
      tone: sandboxTone,
      status: sandboxLabel,
      hint: 'Isolated Docker containers for AI sessions and workflows.',
    },
    {
      title: 'Preview Gateway',
      to: '/agent-sandbox/preview',
      icon: Globe,
      tone: gatewayTone,
      status: gatewayLabel,
      hint: 'Routes preview URLs to dev servers running inside sandboxes.',
    },
    {
      title: 'Secrets',
      to: '/agent-sandbox/secrets',
      icon: KeyRound,
      tone: secretsTone,
      status: secretsLabel,
      hint: 'Encrypted env vars and config files injected into every sandbox.',
    },
  ]

  // Action banner — surface the single most important pending task. We only
  // show it when there's something *blocking* the user from a successful AI
  // session. "Add a secret" doesn't block; "save a credential" does.
  const banner = (() => {
    if (catalogPending) return null
    if (!anyConfigured) {
      return {
        tone: 'bad' as Tone,
        title: 'Save a credential to start',
        body: 'No AI provider has a saved credential yet. AI sessions will fail until one is configured.',
        cta: 'Configure providers',
        to: '/agent-sandbox/providers',
      }
    }
    if (activeProvider && !activeProvider.credential_saved) {
      return {
        tone: 'warn' as Tone,
        title: `${activeProvider.name} is active but unconfigured`,
        body: 'Switch the active provider or save a credential for the current one.',
        cta: 'Fix active provider',
        to: '/agent-sandbox/providers',
      }
    }
    if (sandboxStatus && !sandboxStatus.docker_available) {
      return {
        tone: 'bad' as Tone,
        title: 'Docker is unavailable',
        body: 'Agents always run inside sandboxed containers. Install or start Docker to run sessions.',
        cta: 'Open sandbox settings',
        to: '/agent-sandbox/sandbox',
      }
    }
    if (gatewayStatus && gatewayStatus.present && !gatewayStatus.running) {
      return {
        tone: 'warn' as Tone,
        title: 'Preview gateway is stopped',
        body: 'Workspace previews (ws-*.preview-domain) will not route until the gateway is running.',
        cta: 'Open gateway',
        to: '/agent-sandbox/preview',
      }
    }
    if (gatewayStatus?.drift) {
      return {
        tone: 'warn' as Tone,
        title: 'Preview gateway image drift',
        body: 'The deployed gateway image differs from the configured one. Pull & apply the expected image.',
        cta: 'Open gateway',
        to: '/agent-sandbox/preview',
      }
    }
    return null
  })()

  return (
    <div className="space-y-6">
      {banner && (
        <Card className={toneClass(banner.tone)}>
          <CardContent className="flex flex-col sm:flex-row sm:items-center gap-3 py-4">
            <div className="shrink-0">
              <ToneIcon tone={banner.tone} />
            </div>
            <div className="flex-1 min-w-0">
              <p className="text-sm font-medium">{banner.title}</p>
              <p className="text-sm text-muted-foreground">{banner.body}</p>
            </div>
            <Button asChild size="sm" className="shrink-0">
              <Link to={banner.to}>{banner.cta}</Link>
            </Button>
          </CardContent>
        </Card>
      )}

      <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
        {cards.map((c) => {
          const Icon = c.icon
          return (
            <Link
              key={c.title}
              to={c.to}
              className="group focus:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-lg"
            >
              <Card
                className={`h-full transition-colors hover:border-primary/40 ${toneClass(c.tone)}`}
              >
                <CardHeader className="pb-3">
                  <div className="flex items-start justify-between gap-2">
                    <CardTitle className="text-base flex items-center gap-2">
                      <Icon className="h-4 w-4 text-muted-foreground" />
                      {c.title}
                    </CardTitle>
                    <ArrowRight className="h-4 w-4 text-muted-foreground opacity-0 -translate-x-1 transition group-hover:opacity-100 group-hover:translate-x-0" />
                  </div>
                  <CardDescription className="text-xs">{c.hint}</CardDescription>
                </CardHeader>
                <CardContent>
                  <div className="flex items-center gap-2 text-sm">
                    <ToneIcon tone={c.tone} />
                    <span>{c.status}</span>
                  </div>
                </CardContent>
              </Card>
            </Link>
          )
        })}
      </div>

      {/* Quick concepts — tucked under the cards so it doesn't compete with the
          status surfaces but is still discoverable. */}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">How it fits together</CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-1 sm:grid-cols-2 gap-4 text-sm">
          <div className="flex items-start gap-2">
            <Sparkles className="h-4 w-4 mt-0.5 shrink-0 text-muted-foreground" />
            <div>
              <p className="font-medium">Provider</p>
              <p className="text-muted-foreground text-xs">
                The AI CLI Temps shells out to (Claude Code, Codex, OpenCode).
                One credential per provider, one active at a time.
              </p>
            </div>
          </div>
          <div className="flex items-start gap-2">
            <Container className="h-4 w-4 mt-0.5 shrink-0 text-muted-foreground" />
            <div>
              <p className="font-medium">Sandbox</p>
              <p className="text-muted-foreground text-xs">
                Agents always run inside an isolated Docker container — never on
                the host. Requires Docker to be available.
              </p>
            </div>
          </div>
          <div className="flex items-start gap-2">
            <Globe className="h-4 w-4 mt-0.5 shrink-0 text-muted-foreground" />
            <div>
              <p className="font-medium">Preview gateway</p>
              <p className="text-muted-foreground text-xs">
                A shared proxy container that exposes dev servers running inside
                sandboxes via <code className="bg-muted px-1 rounded">ws-*.preview-domain</code>.
              </p>
            </div>
          </div>
          <div className="flex items-start gap-2">
            <KeyRound className="h-4 w-4 mt-0.5 shrink-0 text-muted-foreground" />
            <div>
              <p className="font-medium">Secrets</p>
              <p className="text-muted-foreground text-xs">
                Encrypted env vars and files injected into every sandbox. Reference
                via <code className="bg-muted px-1 rounded">{'${TEMPS_SECRET:NAME}'}</code>.
              </p>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
