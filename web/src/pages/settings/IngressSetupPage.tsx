// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// "Expose your apps": how visitors reach the workers, on one page. A
// Cloudflare Tunnel is set up end to end here (account, domain, workers,
// hostnames) over the same endpoints as the DNS provider pages and the
// worker's tunnel dialog; open ports stay on each worker's page.

import { zodResolver } from '@hookform/resolvers/zod'
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  Loader2,
  XCircle,
} from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  adminGetNodePublicIngressSettingsOptions,
  adminListNodesOptions,
  adminListNodesQueryKey,
  listDnsProvidersOptions,
  listDnsProvidersQueryKey,
  listIngressConnectorsOptions,
  listIngressProvidersOptions,
  listManagedDomainsOptions,
  listProviderZonesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { createDnsProvider, deleteDnsProvider } from '@/api/client/sdk.gen'
import type {
  ConnectorView,
  DnsProviderResponse,
  DnsZone,
  ManagedDomainResponse,
  NodeInfoResponse,
} from '@/api/client/types.gen'
import {
  CloudflareTokenCheck,
  CloudflareTokenChecklist,
  CloudflareTokenGuide,
} from '@/components/domains/CloudflareTokenGuide'
import {
  deliveryError,
  requireDeliveryData,
} from '@/components/domains/delivery-errors'
import { PageHeader } from '@/components/layout/PageContainer'
import {
  CLOUDFLARE_TUNNEL_KIND,
  type HostnameScheme,
  type IngressWay,
  type SetupOutcome,
  type SetupPlan,
  type SetupStep,
  type UndoResult,
  runTunnelSetup,
  sdkIngressSetupApi,
  subdomainLabelError,
  tunnelDomainFor,
  undoSetup,
  uniqueProviderName,
  uniqueTunnelName,
  workerEligibility,
  type WorkerEligibility,
} from '@/components/nodes/ingress-setup'
import { WorkerIngressBadge } from '@/components/nodes/WorkerIngressBadge'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { ScrollRegion } from '@/components/ui/scroll-region'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { allTunnelsOptions, baseHost } from '@/lib/tunnel-domains'
import { addressScope } from '@/lib/worker-ingress'
import { WORKER_NODES_URL } from '@/lib/worker-nodes'
import { cn } from '@/lib/utils'

const EXAMPLE_APP = 'app'

function workerUrl(id: number) {
  return `${WORKER_NODES_URL}/${id}#public-ingress`
}

export function IngressSetupPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Worker nodes', href: WORKER_NODES_URL },
      { label: 'Expose your apps' },
    ])
  }, [setBreadcrumbs])
  usePageTitle('Expose your apps')

  const [searchParams, setSearchParams] = useSearchParams()
  const via: IngressWay =
    searchParams.get('via') === 'ports' ? 'ports' : 'tunnel'
  const preferredWorker = Number(searchParams.get('worker')) || null

  const nodesQuery = useQuery(adminListNodesOptions())
  const workers = (nodesQuery.data?.nodes ?? []).filter(
    (n) => n.role === 'worker'
  )
  const anyPublic = workers.some((w) => addressScope(w.address) === 'public')

  const choose = (next: IngressWay) => {
    const params = new URLSearchParams(searchParams)
    params.set('via', next)
    setSearchParams(params, { replace: true })
  }

  return (
    <div className="w-full space-y-6">
      <PageHeader
        title="Expose your apps"
        description="Let visitors on the internet reach the apps your workers run. Pick how they get in, then Temps sets it up and shows each step."
      />

      <section className="space-y-3" aria-labelledby="ingress-way">
        <h2 id="ingress-way" className="text-base font-semibold">
          1. How visitors get in
        </h2>
        <RadioGroup
          value={via}
          onValueChange={(value) => choose(value as IngressWay)}
          className="grid grid-cols-1 gap-3 md:grid-cols-2"
        >
          <WayCard
            value="tunnel"
            selected={via === 'tunnel'}
            title="Cloudflare Tunnel"
            badge={!anyPublic ? 'Recommended' : undefined}
          >
            The worker connects out to Cloudflare, and visitors come in through
            Cloudflare&apos;s network. No public IP, open port or router change;
            Cloudflare serves HTTPS. Best when the worker has no public IP.
          </WayCard>
          <WayCard
            value="ports"
            selected={via === 'ports'}
            title="Open ports on a public IP"
          >
            The worker listens on ports 80 and 443 itself and Temps issues
            Let&apos;s Encrypt certificates. Needs a public IP that sends those
            ports to the worker.
          </WayCard>
        </RadioGroup>
      </section>

      {nodesQuery.isLoading ? (
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-24 w-full" />
        </div>
      ) : nodesQuery.isError ? (
        <Alert variant="destructive">
          <AlertTitle>Could not load the worker nodes</AlertTitle>
          <AlertDescription>
            {deliveryError(nodesQuery.error)}{' '}
            <Button
              variant="link"
              className="h-auto p-0"
              onClick={() => void nodesQuery.refetch()}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : workers.length === 0 ? (
        <NoWorkers />
      ) : via === 'ports' ? (
        <OpenPortsPanel workers={workers} />
      ) : (
        <TunnelSetup workers={workers} preferredWorker={preferredWorker} />
      )}
    </div>
  )
}

function WayCard({
  value,
  selected,
  title,
  badge,
  children,
}: {
  value: IngressWay
  selected: boolean
  title: string
  badge?: string
  children: ReactNode
}) {
  return (
    <Label
      htmlFor={`ingress-way-${value}`}
      className={cn(
        'flex cursor-pointer items-start gap-3 rounded-lg border p-4 font-normal',
        selected && 'border-primary bg-primary/5'
      )}
    >
      <RadioGroupItem
        id={`ingress-way-${value}`}
        value={value}
        className="mt-0.5"
      />
      <span className="space-y-1">
        <span className="flex flex-wrap items-center gap-2 text-sm font-medium">
          {title}
          {badge && <Badge>{badge}</Badge>}
        </span>
        <span className="block text-sm text-muted-foreground">{children}</span>
      </span>
    </Label>
  )
}

function NoWorkers() {
  return (
    <Alert>
      <AlertTitle>No worker node yet</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>
          This page lets visitors reach apps on worker nodes. The control plane
          serves the apps it runs itself directly. Add a worker, then come back
          to expose it.
        </p>
        <Button asChild size="sm">
          <Link to={WORKER_NODES_URL}>Add a worker node</Link>
        </Button>
      </AlertDescription>
    </Alert>
  )
}

function OpenPortsPanel({ workers }: { workers: NodeInfoResponse[] }) {
  return (
    <Card className="shadow-none">
      <CardHeader>
        <CardTitle className="text-base">2. Open ports on a worker</CardTitle>
        <CardDescription>
          Each worker chooses where it listens and checks that the internet
          reaches it, on its own page. Point your domain&apos;s DNS at the
          worker&apos;s public IP once the check passes.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <ul className="divide-y rounded-md border">
          {workers.map((node) => (
            <li
              key={node.id}
              className="flex flex-wrap items-center justify-between gap-2 p-3"
            >
              <div className="flex min-w-0 flex-wrap items-center gap-2 text-sm">
                <span className="font-medium">{node.name}</span>
                <span className="text-muted-foreground">
                  {node.address} · {node.status}
                </span>
                <WorkerIngressBadge node={node} linkSetup={false} />
              </div>
              <Button asChild size="sm" variant="outline">
                <Link to={workerUrl(node.id)}>
                  Set up open ports
                  <span className="sr-only"> on {node.name}</span>
                </Link>
              </Button>
            </li>
          ))}
        </ul>
      </CardContent>
    </Card>
  )
}

// ----- Cloudflare Tunnel ----------------------------------------------------

type CredentialMode = 'existing' | 'token'

interface ConnectedAccount {
  id: number
  name: string
}

function TunnelSetup({
  workers,
  preferredWorker,
}: {
  workers: NodeInfoResponse[]
  preferredWorker: number | null
}) {
  const queryClient = useQueryClient()
  const providersQuery = useQuery(listIngressProvidersOptions())
  const dnsProvidersQuery = useQuery(listDnsProvidersOptions())
  const tunnelsQuery = useQuery({
    ...allTunnelsOptions(),
    refetchInterval: 10_000,
  })
  const accounts = (dnsProvidersQuery.data ?? []).filter(
    (p) => p.is_active && p.provider_type.toLowerCase() === 'cloudflare'
  )
  const tunnels = tunnelsQuery.data ?? []

  // `null` until the operator picks: then the defaults apply.
  const [modeChoice, setModeChoice] = useState<CredentialMode | null>(null)
  const [existingChoice, setExistingChoice] = useState<number | null>(null)
  const [connected, setConnected] = useState<ConnectedAccount | null>(null)
  const [zone, setZone] = useState('')
  const [scheme, setScheme] = useState<HostnameScheme['kind']>('zone')
  const [label, setLabel] = useState('')
  const [picked, setPicked] = useState<number[] | null>(null)
  const [steps, setSteps] = useState<SetupStep[] | null>(null)
  const abort = useRef<AbortController | null>(null)
  useEffect(() => () => abort.current?.abort(), [])

  const mode: CredentialMode =
    modeChoice ?? (accounts.length > 0 ? 'existing' : 'token')
  const existing =
    accounts.find((a) => a.id === existingChoice) ??
    (accounts.length === 1 ? accounts[0] : undefined)
  const account: (ConnectedAccount & { createdThisRun: boolean }) | null =
    mode === 'token'
      ? connected && { ...connected, createdThisRun: true }
      : existing
        ? { id: existing.id, name: existing.name, createdThisRun: false }
        : null

  const settingsQueries = useQueries({
    queries: workers.map((node) => ({
      ...adminGetNodePublicIngressSettingsOptions({
        path: { node_id: node.id },
      }),
      enabled: node.status === 'active',
      retry: false,
    })),
  })
  const eligibility = new Map<number, WorkerEligibility>(
    workers.map((node, index) => [
      node.id,
      workerEligibility(node, settingsQueries[index]?.data?.mode, tunnels),
    ])
  )
  const defaults =
    preferredWorker != null && eligibility.get(preferredWorker)?.selectable
      ? [preferredWorker]
      : workers.filter((n) => eligibility.get(n.id)?.defaultOn).map((n) => n.id)
  const nodeIds = (picked ?? defaults).filter(
    (id) => eligibility.get(id)?.selectable
  )
  const chosenNodes = workers.filter((n) => nodeIds.includes(n.id))

  const managedQuery = useQuery({
    ...listManagedDomainsOptions({ path: { id: account?.id ?? 0 } }),
    enabled: account != null,
  })
  const managed = (managedQuery.data ?? []).find(
    (d) => d.domain.toLowerCase() === zone.toLowerCase()
  )
  const schemeValue: HostnameScheme =
    scheme === 'zone' ? { kind: 'zone' } : { kind: 'subdomain', label }
  const labelError =
    scheme === 'subdomain' ? subdomainLabelError(label) : undefined
  const tunnelDomain =
    zone && !labelError ? tunnelDomainFor(zone, schemeValue) : ''
  const conflict = tunnelDomain
    ? tunnels.find((t) => baseHost(t.domain) === tunnelDomain)
    : undefined

  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({
        queryKey: listIngressConnectorsOptions().queryKey,
      }),
      queryClient.invalidateQueries({ queryKey: listDnsProvidersQueryKey() }),
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] }),
      queryClient.invalidateQueries({ queryKey: adminListNodesQueryKey() }),
      account &&
        queryClient.invalidateQueries({
          queryKey: listManagedDomainsOptions({ path: { id: account.id } })
            .queryKey,
        }),
    ])

  const run = useMutation({
    mutationFn: (plan: SetupPlan) => {
      abort.current?.abort()
      abort.current = new AbortController()
      return runTunnelSetup(
        plan,
        sdkIngressSetupApi,
        setSteps,
        abort.current.signal
      )
    },
    onSuccess: async (outcome, plan) => {
      if (outcome.kind === 'failed') {
        // The account it added is gone again: the token can be reconnected.
        const removed = outcome.undo.some((u) => u.ok && u.what === 'provider')
        if (removed) setConnected(null)
        toast.error('Tunnel setup failed', { description: outcome.error })
      } else {
        if (outcome.kind === 'done') {
          toast.success(`Apps under ${outcome.connector.domain} are reachable`)
        }
        // The tunnel this run kept now uses the account the pasted token
        // added, so from here on it is an existing account like any other:
        // "Use a different token" (which deletes it) must not be offered for
        // it, or one click would leave the tunnel without its credentials.
        if (plan.provider.createdThisRun) {
          setModeChoice('existing')
          setExistingChoice(plan.provider.id)
          setConnected(null)
        }
      }
      await refresh()
    },
    onError: (error) =>
      toast.error('Tunnel setup stopped', {
        description: deliveryError(error),
      }),
  })
  const undo = useMutation({
    mutationFn: (outcome: SetupOutcome) =>
      undoSetup(outcome.created, sdkIngressSetupApi),
    onSuccess: async (results) => {
      if (results.every((r) => r.ok)) {
        toast.success('Removed what this page created')
      } else {
        toast.error('Some of it could not be removed')
      }
      // The account the run adopted is gone: fall back to the defaults.
      if (results.some((r) => r.ok && r.what === 'provider')) {
        setConnected(null)
        setExistingChoice(null)
        setModeChoice(null)
      }
      await refresh()
    },
    onError: (error) =>
      toast.error('Could not undo', { description: deliveryError(error) }),
  })

  const locked = run.isPending || (run.isSuccess && run.data.kind !== 'failed')
  const ready =
    account != null &&
    !!zone &&
    !labelError &&
    nodeIds.length > 0 &&
    !conflict &&
    tunnelsQuery.isSuccess &&
    !managedQuery.isLoading
  const tunnelName = tunnelDomain
    ? uniqueTunnelName(
        tunnelDomain,
        tunnels.map((t) => t.name)
      )
    : ''

  const start = () => {
    if (!ready || !account) return
    run.mutate({
      provider: account,
      zone,
      managed,
      tunnelDomain,
      tunnelName,
      nodes: chosenNodes.map((n) => ({ id: n.id, name: n.name })),
    })
  }
  const outcome = run.data
  const startOver = () => {
    run.reset()
    undo.reset()
    setSteps(null)
  }

  if (providersQuery.isSuccess) {
    const offered = providersQuery.data.some(
      (p) => p.kind === CLOUDFLARE_TUNNEL_KIND
    )
    if (!offered) {
      return (
        <Alert>
          <AlertTitle>Cloudflare Tunnel isn&apos;t available here</AlertTitle>
          <AlertDescription>
            This Temps doesn&apos;t offer Cloudflare Tunnel. Upgrade the control
            plane, or use open ports on a public IP instead.
          </AlertDescription>
        </Alert>
      )
    }
  }

  return (
    <div className="space-y-6">
      <fieldset disabled={locked} className="space-y-6">
        <Card className="shadow-none">
          <CardHeader>
            <CardTitle className="text-base">2. Cloudflare account</CardTitle>
            <CardDescription>
              Temps uses an API token to create the tunnel and the DNS record
              for each app. Your domain must be on Cloudflare (the free plan
              works).
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {dnsProvidersQuery.isLoading ? (
              <Skeleton className="h-16 w-full" />
            ) : (
              <CredentialSection
                mode={mode}
                onMode={(next) => {
                  setModeChoice(next)
                  setZone('')
                }}
                accounts={accounts}
                existing={existing}
                onExisting={(id) => {
                  setExistingChoice(id)
                  setZone('')
                }}
                connected={connected}
                onConnected={(next) => {
                  setConnected(next)
                  setZone('')
                }}
              />
            )}
          </CardContent>
        </Card>

        <Card className="shadow-none">
          <CardHeader>
            <CardTitle className="text-base">3. Domain</CardTitle>
            <CardDescription>
              The zones this token can see in Cloudflare. Apps get hostnames
              under the one you pick.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {account ? (
              <ZonePicker
                providerId={account.id}
                managed={managedQuery.data ?? []}
                value={zone}
                onChange={setZone}
              />
            ) : (
              <p className="text-sm text-muted-foreground">
                Choose or connect a Cloudflare account first.
              </p>
            )}
            {zone && (
              <SchemePicker
                zone={zone}
                scheme={scheme}
                onScheme={setScheme}
                label={label}
                onLabel={setLabel}
                labelError={labelError}
              />
            )}
            {conflict && <ConflictNote tunnel={conflict} />}
          </CardContent>
        </Card>

        <Card className="shadow-none">
          <CardHeader>
            <CardTitle className="text-base">4. Workers</CardTitle>
            <CardDescription>
              Every worker picked runs the tunnel. With more than one, apps stay
              reachable when a worker goes down.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <WorkerPicker
              workers={workers}
              eligibility={eligibility}
              selected={nodeIds}
              onChange={setPicked}
            />
          </CardContent>
        </Card>
      </fieldset>

      <Card className="shadow-none">
        <CardHeader>
          <CardTitle className="text-base">5. Create</CardTitle>
          <CardDescription>
            {ready
              ? `Creates tunnel ${tunnelName} in Cloudflare for ${tunnelDomain}, running on ${chosenNodes.map((n) => n.name).join(', ')}.${managed ? '' : ` Adds ${zone} to Temps first.`}`
              : 'Pick an account, a domain and at least one worker.'}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {steps && <StepList steps={steps} />}
          {outcome && (
            <OutcomePanel
              outcome={outcome}
              addedAccount={
                run.variables?.provider.createdThisRun
                  ? run.variables.provider.name
                  : undefined
              }
              undoResults={undo.data}
              undoing={undo.isPending}
              onUndo={() => undo.mutate(outcome)}
            />
          )}
          <div className="flex flex-wrap gap-2">
            {!run.data && (
              <Button onClick={start} disabled={!ready || run.isPending}>
                {run.isPending && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                {run.isPending ? 'Setting up…' : 'Create tunnel'}
              </Button>
            )}
            {run.data && (
              <Button variant="outline" onClick={startOver}>
                {run.data.kind === 'failed'
                  ? 'Change and try again'
                  : 'Set up another'}
              </Button>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

function CredentialSection({
  mode,
  onMode,
  accounts,
  existing,
  onExisting,
  connected,
  onConnected,
}: {
  mode: CredentialMode
  onMode: (mode: CredentialMode) => void
  accounts: DnsProviderResponse[]
  existing: DnsProviderResponse | undefined
  onExisting: (id: number) => void
  connected: ConnectedAccount | null
  onConnected: (account: ConnectedAccount | null) => void
}) {
  return (
    <div className="space-y-4">
      <RadioGroup
        value={mode}
        onValueChange={(value) => onMode(value as CredentialMode)}
        className="grid grid-cols-1 gap-2 md:grid-cols-2"
      >
        <Label
          htmlFor="credential-existing"
          className={cn(
            'flex items-start gap-2 rounded-md border p-3 font-normal',
            accounts.length === 0
              ? 'cursor-not-allowed opacity-60'
              : 'cursor-pointer'
          )}
        >
          <RadioGroupItem
            id="credential-existing"
            value="existing"
            disabled={accounts.length === 0}
            className="mt-0.5"
          />
          <span className="text-sm">
            Use a Cloudflare account already in Temps
            <span className="block text-xs text-muted-foreground">
              {accounts.length === 0
                ? 'None yet.'
                : `${accounts.length} available`}
            </span>
          </span>
        </Label>
        <Label
          htmlFor="credential-token"
          className="flex cursor-pointer items-start gap-2 rounded-md border p-3 font-normal"
        >
          <RadioGroupItem
            id="credential-token"
            value="token"
            className="mt-0.5"
          />
          <span className="text-sm">
            Paste an API token
            <span className="block text-xs text-muted-foreground">
              Temps checks it with Cloudflare, then adds the account.
            </span>
          </span>
        </Label>
      </RadioGroup>

      {mode === 'existing' ? (
        <RadioGroup
          value={existing ? String(existing.id) : ''}
          onValueChange={(value) => onExisting(Number(value))}
          aria-label="Cloudflare account"
          className="grid grid-cols-1 gap-2 md:grid-cols-2 lg:grid-cols-3"
        >
          {accounts.map((p) => (
            <Label
              key={p.id}
              htmlFor={`account-${p.id}`}
              className="flex cursor-pointer items-start gap-2 rounded-md border p-3 font-normal"
            >
              <RadioGroupItem
                id={`account-${p.id}`}
                value={String(p.id)}
                className="mt-0.5"
              />
              <span className="min-w-0 text-sm">
                {p.name}
                {p.last_error && (
                  <span className="block text-xs text-destructive">
                    {p.last_error}
                  </span>
                )}
              </span>
            </Label>
          ))}
          <p className="text-xs text-muted-foreground md:col-span-2 lg:col-span-3">
            Its token needs <code>Account · Cloudflare Tunnel · Edit</code> as
            well as the DNS permissions. If creating the tunnel is refused, add
            that permission to the token in Cloudflare; the token keeps its
            value.
          </p>
        </RadioGroup>
      ) : connected ? (
        <ConnectedNote account={connected} onReplace={onConnected} />
      ) : (
        <TokenForm accounts={accounts} onConnected={onConnected} />
      )}
    </div>
  )
}

const tokenSchema = z.object({
  name: z.string().trim().min(1, 'Give the account a name.'),
  api_token: z.string().trim().min(1, 'Paste the API token.'),
  account_id: z.string().trim().optional(),
})
type TokenValues = z.infer<typeof tokenSchema>

function TokenForm({
  accounts,
  onConnected,
}: {
  accounts: DnsProviderResponse[]
  onConnected: (account: ConnectedAccount) => void
}) {
  const queryClient = useQueryClient()
  const form = useForm<TokenValues>({
    resolver: zodResolver(tokenSchema),
    defaultValues: {
      name: uniqueProviderName(
        'Cloudflare',
        accounts.map((a) => a.name)
      ),
      api_token: '',
      account_id: '',
    },
  })
  const connect = useMutation({
    // The server tests the token with Cloudflare before saving anything.
    mutationFn: async (values: TokenValues) =>
      requireDeliveryData(
        await createDnsProvider({
          body: {
            name: values.name,
            provider_type: 'cloudflare',
            description: 'Added by Expose your apps',
            credentials: {
              type: 'cloudflare',
              api_token: values.api_token,
              account_id: values.account_id || null,
            },
          },
        })
      ),
    onSuccess: async (provider) => {
      toast.success('Cloudflare accepted the token')
      onConnected({ id: provider.id, name: provider.name })
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: listDnsProvidersQueryKey() }),
        queryClient.invalidateQueries({ queryKey: ['dnsProviders'] }),
      ])
    },
    onError: (error) =>
      toast.error('Cloudflare refused the token', {
        description: deliveryError(error),
      }),
  })

  return (
    <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
      <Form {...form}>
        <form
          onSubmit={form.handleSubmit((values) => connect.mutate(values))}
          className="space-y-4"
        >
          <FormField
            control={form.control}
            name="api_token"
            render={({ field }) => (
              <FormItem>
                <FormLabel>API token</FormLabel>
                <FormControl>
                  <Input type="password" autoComplete="off" {...field} />
                </FormControl>
                <FormMessage />
                <CloudflareTokenCheck token={field.value} />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="name"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Name in Temps</FormLabel>
                <FormControl>
                  <Input autoComplete="off" {...field} />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="account_id"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Account ID (optional)</FormLabel>
                <FormControl>
                  <Input autoComplete="off" {...field} />
                </FormControl>
                <FormDescription>
                  Only needed when the token reaches more than one Cloudflare
                  account. It is in your Cloudflare dashboard URL.
                </FormDescription>
                <FormMessage />
              </FormItem>
            )}
          />
          {connect.isError && (
            <Alert variant="destructive">
              <AlertDescription>
                {deliveryError(connect.error)}
              </AlertDescription>
            </Alert>
          )}
          <Button type="submit" disabled={connect.isPending}>
            {connect.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Check token and continue
          </Button>
        </form>
      </Form>
      <CloudflareTokenGuide tunnel="required" />
    </div>
  )
}

function ConnectedNote({
  account,
  onReplace,
}: {
  account: ConnectedAccount
  onReplace: (account: ConnectedAccount | null) => void
}) {
  const queryClient = useQueryClient()
  const [confirming, setConfirming] = useState(false)
  // Added on this page a moment ago and not used by any tunnel yet (a
  // finished run turns it into an existing account), so replacing the token
  // removes it.
  const remove = useMutation({
    mutationFn: async () => {
      const { error } = await deleteDnsProvider({ path: { id: account.id } })
      if (error) throw new Error(deliveryError(error))
    },
    onSuccess: async () => {
      onReplace(null)
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: listDnsProvidersQueryKey() }),
        queryClient.invalidateQueries({ queryKey: ['dnsProviders'] }),
      ])
    },
    onError: (error) => {
      setConfirming(false)
      toast.error(`Could not remove ${account.name}`, {
        description: deliveryError(error),
      })
    },
  })
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 rounded-md border p-3">
      <p className="flex items-center gap-2 text-sm">
        <CheckCircle2 className="h-4 w-4 text-green-600" />
        Cloudflare accepted the token. Added as <strong>{account.name}</strong>.
      </p>
      <Button
        size="sm"
        variant="ghost"
        disabled={remove.isPending}
        onClick={() => setConfirming(true)}
      >
        Use a different token
      </Button>
      <ConfirmDeleteDialog
        open={confirming}
        onOpenChange={setConfirming}
        resourceType="Cloudflare account"
        resourceName={account.name}
        verb="Remove"
        consequences="Temps forgets this account and its API token so you can paste a different one. Nothing set up on this page uses it yet."
        isPending={remove.isPending}
        onConfirm={() => remove.mutate()}
      />
    </div>
  )
}

function ZonePicker({
  providerId,
  managed,
  value,
  onChange,
}: {
  providerId: number
  managed: ManagedDomainResponse[]
  value: string
  onChange: (zone: string) => void
}) {
  const zonesQuery = useQuery({
    ...listProviderZonesOptions({ path: { id: providerId } }),
    retry: false,
  })
  if (zonesQuery.isLoading) {
    return (
      <div className="grid grid-cols-1 gap-2 md:grid-cols-2" aria-busy="true">
        <Skeleton className="h-14 w-full" />
        <Skeleton className="h-14 w-full" />
      </div>
    )
  }
  if (zonesQuery.isError) {
    return (
      <Alert variant="destructive">
        <AlertTitle>Cloudflare didn&apos;t list the zones</AlertTitle>
        <AlertDescription className="space-y-2">
          <p>{deliveryError(zonesQuery.error)}</p>
          <CloudflareTokenChecklist className="bg-background" />
          <p>
            Edit the token in Cloudflare to match; it keeps its value.{' '}
            <Button
              variant="link"
              className="h-auto p-0"
              onClick={() => void zonesQuery.refetch()}
            >
              Try again
            </Button>
          </p>
        </AlertDescription>
      </Alert>
    )
  }
  const zones = zonesQuery.data?.zones ?? []
  if (zones.length === 0) {
    return (
      <Alert>
        <AlertTitle>The token can&apos;t see any zone</AlertTitle>
        <AlertDescription>
          Edit the token in Cloudflare: under Zone Resources, include the zone
          of the domain your apps should use. It keeps its value, so nothing
          changes here.{' '}
          <Button
            variant="link"
            className="h-auto p-0"
            onClick={() => void zonesQuery.refetch()}
          >
            Check again
          </Button>
        </AlertDescription>
      </Alert>
    )
  }
  return (
    <ScrollRegion
      className="max-h-80 overflow-y-auto"
      regionLabel="Zones, scrollable"
    >
      <RadioGroup
        value={value}
        onValueChange={onChange}
        aria-label="Domain"
        className="grid grid-cols-1 gap-2 md:grid-cols-2"
      >
        {zones.map((zone) => (
          <ZoneOption
            key={zone.id}
            zone={zone}
            managed={managed.find(
              (d) => d.domain.toLowerCase() === zone.name.toLowerCase()
            )}
          />
        ))}
      </RadioGroup>
    </ScrollRegion>
  )
}

function ZoneOption({
  zone,
  managed,
}: {
  zone: DnsZone
  managed: ManagedDomainResponse | undefined
}) {
  const id = `zone-${zone.id}`
  return (
    <Label
      htmlFor={id}
      className="flex cursor-pointer items-start gap-2 rounded-md border p-3 font-normal"
    >
      <RadioGroupItem id={id} value={zone.name} className="mt-0.5" />
      <span className="min-w-0 space-y-1 text-sm">
        <span className="flex flex-wrap items-center gap-2">
          <span className="font-medium break-all">{zone.name}</span>
          {managed && (
            <Badge variant="outline">
              {managed.verified ? 'In Temps' : 'In Temps, not verified'}
            </Badge>
          )}
          {zone.status !== 'active' && (
            <Badge
              variant="outline"
              className="border-amber-500/40 text-amber-700 dark:text-amber-400"
            >
              {zone.status}
            </Badge>
          )}
        </span>
        {zone.status !== 'active' && (
          <span className="block text-xs text-muted-foreground">
            Cloudflare isn&apos;t serving this zone yet: point the domain&apos;s
            nameservers at Cloudflare first.
          </span>
        )}
      </span>
    </Label>
  )
}

function SchemePicker({
  zone,
  scheme,
  onScheme,
  label,
  onLabel,
  labelError,
}: {
  zone: string
  scheme: HostnameScheme['kind']
  onScheme: (scheme: HostnameScheme['kind']) => void
  label: string
  onLabel: (label: string) => void
  labelError: string | undefined
}) {
  const sub = label.trim().toLowerCase() || 'lab'
  return (
    <div className="space-y-3">
      <p className="text-sm font-medium">Hostnames</p>
      <RadioGroup
        value={scheme}
        onValueChange={(value) => onScheme(value as HostnameScheme['kind'])}
        aria-label="Hostnames"
        className="grid gap-2"
      >
        <Label
          htmlFor="scheme-zone"
          className="flex cursor-pointer items-start gap-2 font-normal"
        >
          <RadioGroupItem id="scheme-zone" value="zone" className="mt-0.5" />
          <span className="text-sm">
            <code>
              {EXAMPLE_APP}.{zone}
            </code>{' '}
            <span className="text-muted-foreground">
              (recommended: Cloudflare&apos;s free certificate covers it)
            </span>
          </span>
        </Label>
        <Label
          htmlFor="scheme-subdomain"
          className="flex cursor-pointer items-start gap-2 font-normal"
        >
          <RadioGroupItem
            id="scheme-subdomain"
            value="subdomain"
            className="mt-0.5"
          />
          <span className="text-sm">
            Under a subdomain, such as{' '}
            <code>
              {EXAMPLE_APP}.{sub}.{zone}
            </code>
          </span>
        </Label>
      </RadioGroup>
      {scheme === 'subdomain' && (
        <div className="space-y-3 pl-6">
          <div className="space-y-1">
            <Label htmlFor="scheme-label">Subdomain</Label>
            <div className="flex items-center gap-2">
              <Input
                id="scheme-label"
                className="w-40"
                placeholder="lab"
                autoComplete="off"
                value={label}
                aria-invalid={!!label && !!labelError}
                onChange={(e) => onLabel(e.target.value)}
              />
              <span className="text-sm text-muted-foreground">.{zone}</span>
            </div>
            {label && labelError && (
              <p className="text-sm text-destructive">{labelError}</p>
            )}
          </div>
          <Alert className="border-amber-500/30 bg-amber-500/5">
            <AlertTriangle className="h-4 w-4 text-amber-500" />
            <AlertTitle className="text-amber-700 dark:text-amber-400">
              HTTPS fails without Advanced Certificate Manager
            </AlertTitle>
            <AlertDescription className="text-amber-700 dark:text-amber-300">
              Cloudflare&apos;s free Universal SSL covers {zone} and one level
              below it (<code>*.{zone}</code>). Names such as{' '}
              <code>
                {EXAMPLE_APP}.{sub}.{zone}
              </code>{' '}
              are two levels down, so visitors get a certificate error unless
              the zone has Advanced Certificate Manager (or Total TLS) covering{' '}
              <code>
                *.{sub}.{zone}
              </code>
              .
            </AlertDescription>
          </Alert>
        </div>
      )}
    </div>
  )
}

function ConflictNote({ tunnel }: { tunnel: ConnectorView }) {
  const first = tunnel.nodes[0]
  return (
    <Alert>
      <AlertTitle>
        Tunnel {tunnel.name} already serves {tunnel.domain}
      </AlertTitle>
      <AlertDescription>
        A second tunnel for the same domain would never carry traffic. To run it
        on more workers, open a worker&apos;s page and choose it under
        &ldquo;Run an existing tunnel here&rdquo;
        {first && (
          <>
            {' '}
            (it runs on{' '}
            <Link className="underline" to={workerUrl(first.node_id)}>
              {first.node_name}
            </Link>
            )
          </>
        )}
        .
      </AlertDescription>
    </Alert>
  )
}

function WorkerPicker({
  workers,
  eligibility,
  selected,
  onChange,
}: {
  workers: NodeInfoResponse[]
  eligibility: Map<number, WorkerEligibility>
  selected: number[]
  onChange: (ids: number[]) => void
}) {
  const allServed = workers.every(
    (n) =>
      !eligibility.get(n.id)?.selectable || eligibility.get(n.id)?.hasIngress
  )
  const toggle = (id: number, on: boolean) =>
    onChange(on ? [...selected, id] : selected.filter((n) => n !== id))
  return (
    <div className="space-y-3">
      {allServed && selected.length === 0 && (
        <p className="text-sm text-muted-foreground">
          Every worker that can take a tunnel already has a way in. Pick the
          ones that should also run this one.
        </p>
      )}
      <ul className="divide-y rounded-md border">
        {workers.map((node) => {
          const state = eligibility.get(node.id)
          const id = `worker-${node.id}`
          return (
            <li key={node.id} className="space-y-2 p-3">
              <div className="flex flex-wrap items-center gap-2">
                <Checkbox
                  id={id}
                  checked={selected.includes(node.id)}
                  disabled={!state?.selectable}
                  onCheckedChange={(on) => toggle(node.id, on === true)}
                />
                <Label htmlFor={id} className="font-medium">
                  {node.name}
                </Label>
                <span className="text-xs text-muted-foreground">
                  {node.address} · {node.status}
                </span>
                <WorkerIngressBadge node={node} linkSetup={false} />
              </div>
              {state?.reason && (
                <div className="space-y-1 pl-6 text-xs text-muted-foreground">
                  <p>{state.reason}</p>
                  {state.command && (
                    <div className="flex items-center gap-2">
                      <span>Run this as root on it:</span>
                      <code className="rounded bg-muted px-1.5 py-0.5">
                        {state.command}
                      </code>
                      <CopyButton
                        minimal
                        className="h-6 w-6"
                        value={state.command}
                      />
                    </div>
                  )}
                </div>
              )}
            </li>
          )
        })}
      </ul>
    </div>
  )
}

function StepIcon({ status }: { status: SetupStep['status'] }) {
  switch (status) {
    case 'done':
      return <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-green-600" />
    case 'failed':
      return <XCircle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
    case 'running':
      return (
        <Loader2 className="mt-0.5 h-4 w-4 shrink-0 animate-spin text-muted-foreground" />
      )
    default:
      return (
        <Circle className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
      )
  }
}

function StepList({ steps }: { steps: SetupStep[] }) {
  return (
    <ol className="space-y-2" aria-label="Progress" aria-live="polite">
      {steps.map((step) => (
        <li key={step.key} className="flex items-start gap-2 text-sm">
          <StepIcon status={step.status} />
          <span className="min-w-0">
            <span
              className={cn(
                step.status === 'pending' && 'text-muted-foreground'
              )}
            >
              {step.label}
            </span>
            {step.detail && (
              <span
                className={cn(
                  'block text-xs',
                  step.status === 'failed'
                    ? 'text-destructive'
                    : 'text-muted-foreground'
                )}
              >
                {step.detail}
              </span>
            )}
          </span>
        </li>
      ))}
    </ol>
  )
}

function UndoList({ results }: { results: UndoResult[] }) {
  if (results.length === 0) {
    return <p>Nothing was created, so nothing needed undoing.</p>
  }
  return (
    <ul className="space-y-1">
      {results.map((r) => (
        <li key={r.label} className="flex items-start gap-2">
          {r.ok ? (
            <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-green-600" />
          ) : (
            <XCircle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
          )}
          <span>
            {r.ok
              ? r.label
              : `Not undone: ${r.label.toLowerCase()}. ${r.error}`}
          </span>
        </li>
      ))}
    </ul>
  )
}

function OutcomePanel({
  outcome,
  addedAccount,
  undoResults,
  undoing,
  onUndo,
}: {
  outcome: SetupOutcome
  /** The Cloudflare account this run added from a pasted token, if any. */
  addedAccount: string | undefined
  undoResults: UndoResult[] | undefined
  undoing: boolean
  onUndo: () => void
}) {
  const [confirmUndo, setConfirmUndo] = useState(false)
  if (outcome.kind === 'failed') {
    const leftovers = outcome.undo.some((r) => !r.ok)
    return (
      <Alert variant="destructive">
        <AlertTitle>Setup failed: {outcome.error}</AlertTitle>
        <AlertDescription className="space-y-2">
          <p>What this page had created is undone:</p>
          <UndoList results={outcome.undo} />
          {leftovers && (
            <p>
              Remove what is left from its{' '}
              <Link className="underline" to="/dns-providers">
                DNS provider
              </Link>{' '}
              or the worker&apos;s page.
            </p>
          )}
        </AlertDescription>
      </Alert>
    )
  }
  if (outcome.kind === 'aborted') return null
  const domain = outcome.connector.domain
  const workers = outcome.connector.nodes
  if (outcome.kind === 'done') {
    return (
      <Alert className="border-green-600/30 bg-green-600/5">
        <CheckCircle2 className="h-4 w-4 text-green-600" />
        <AlertTitle>Visitors can reach apps under {domain}</AlertTitle>
        <AlertDescription className="space-y-2">
          <p>
            Each app with a hostname under it, such as{' '}
            <code>
              {EXAMPLE_APP}.{domain}
            </code>
            , is published through the tunnel automatically. Give an app its
            hostname under{' '}
            <Link className="underline" to="/domains">
              Domains
            </Link>
            .
          </p>
          <WorkerLinks nodes={workers} />
        </AlertDescription>
      </Alert>
    )
  }
  const waiting = workers
    .filter((n) => n.state !== 'ready')
    .map((n) => n.node_name)
  return (
    <Alert className="border-amber-500/30 bg-amber-500/5">
      <AlertTriangle className="h-4 w-4 text-amber-500" />
      <AlertTitle className="text-amber-700 dark:text-amber-400">
        The tunnel is created, but {waiting.join(', ')} hasn&apos;t connected
        yet
      </AlertTitle>
      <AlertDescription className="space-y-2 text-amber-700 dark:text-amber-300">
        <p>
          A worker connects as soon as its agent picks the tunnel up; follow it
          on the worker&apos;s page. If it never does, undo what this page
          created and try another worker.
        </p>
        <WorkerLinks nodes={workers} />
        {undoResults ? (
          <UndoList results={undoResults} />
        ) : (
          <Button
            size="sm"
            variant="outline"
            disabled={undoing}
            onClick={() => setConfirmUndo(true)}
          >
            {undoing && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            Undo what this page created
          </Button>
        )}
        <ConfirmDeleteDialog
          open={confirmUndo}
          onOpenChange={setConfirmUndo}
          resourceType="tunnel"
          resourceName={outcome.connector.name}
          verb="Remove"
          consequences={
            <ul className="list-disc space-y-1 pl-5">
              <li>Its DNS records in Cloudflare go with it.</li>
              {outcome.created.managedDomain && (
                <li>
                  {outcome.created.managedDomain.domain} is removed from Temps.
                </li>
              )}
              {outcome.created.autoManageEnabled && (
                <li>
                  Automatic DNS for {outcome.created.autoManageEnabled.domain}{' '}
                  is turned back off.
                </li>
              )}
              {outcome.created.providerId != null && (
                <li>
                  The Cloudflare account{' '}
                  {addedAccount ? <strong>{addedAccount}</strong> : 'it'} added
                  on this page is removed from Temps.
                </li>
              )}
            </ul>
          }
          onConfirm={() => {
            setConfirmUndo(false)
            onUndo()
          }}
        />
      </AlertDescription>
    </Alert>
  )
}

function WorkerLinks({ nodes }: { nodes: ConnectorView['nodes'] }) {
  return (
    <p className="flex flex-wrap gap-x-3 gap-y-1">
      <span>Workers:</span>
      {nodes.map((n) => (
        <Link key={n.node_id} className="underline" to={workerUrl(n.node_id)}>
          {n.node_name}
        </Link>
      ))}
    </p>
  )
}

export default IngressSetupPage
