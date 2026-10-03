// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle2,
  ExternalLink,
  Globe,
  Key,
  KeyRound,
  Link2,
  Loader2,
  Network,
  ShieldCheck,
  X,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  adminListNodesOptions,
  nodePairingCancelMutation,
  nodePairingCreateMutation,
  nodePairingListOptions,
  wireguardMeshEnableMutation,
  wireguardMeshStatusGetOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  NodePairingResponse,
  WireguardMeshCheck,
  WireguardMeshNodeConnection,
  WireguardMeshStatusResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { SshEnrollNode } from '@/components/nodes/SshEnrollNode'
import {
  QueryErrorAlert,
  TONE_CLASSES,
  WithCode,
} from '@/components/nodes/mesh-ui'
import { problemDetail } from '@/lib/api-problem'
import {
  defaultInternetMethod,
  defaultJoinPath,
  joinCommand,
  joinUrl,
  joinUrlReachableFromOutside,
  meshConnectionLabel,
  meshProblems,
  pairingAsOf,
  pairingEnded,
  pairingProgress,
  pendingPairings,
  type InternetJoinMethod,
  type JoinPath,
} from '@/lib/wireguard-mesh'

const INSTALL_COMMAND = 'curl -fsSL https://temps.sh/install.sh | bash'
const AGENT_SERVICE_COMMAND = 'temps agent service install'
const WITHOUT_SYSTEMD = (
  <>
    Without systemd, run <code>temps agent</code> under your own supervisor
    instead.
  </>
)

/** Mesh state for the Worker Nodes page; polls faster while it comes up. */
export function useWireguardMesh() {
  return useQuery({
    ...wireguardMeshStatusGetOptions(),
    refetchInterval: (query) =>
      query.state.data?.state === 'starting' ? 5_000 : 30_000,
  })
}

function CommandLine({ command }: { command: string }) {
  return (
    <div className="mt-1 flex items-center gap-2 rounded-md bg-muted px-3 py-2 font-mono text-xs">
      <span className="flex-1 overflow-x-auto whitespace-nowrap">
        {command}
      </span>
      <CopyButton minimal className="h-6 w-6 shrink-0" value={command} />
    </div>
  )
}

function Step({
  number,
  title,
  children,
}: {
  number: number
  title: string
  children: ReactNode
}) {
  return (
    <div>
      <p className="font-medium text-foreground">
        {number}. {title}
      </p>
      {children}
    </div>
  )
}

/**
 * Set when this control plane has no join token yet: the `temps join` steps
 * ask for one instead of showing a command with a placeholder that has no
 * value. Pairing and adding a server over SSH do not need it.
 */
export type MissingJoinToken = {
  onGenerate: () => void
  generating: boolean
}

function Skeletons() {
  return (
    <div className="space-y-2">
      <Skeleton className="h-4 w-3/4" />
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
        <Skeleton className="h-8" />
        <Skeleton className="h-8" />
        <Skeleton className="h-8" />
      </div>
      <Skeleton className="h-24 w-full" />
    </div>
  )
}

/** In place of a `temps join` command while there is no join token. */
function GenerateTokenFirst({ missing }: { missing: MissingJoinToken }) {
  return (
    <div className="mt-1 space-y-2 rounded-md border bg-background p-3">
      <p>
        Generate a join token first: the command includes it, and the worker
        presents it to register.
      </p>
      <Button
        type="button"
        size="sm"
        onClick={missing.onGenerate}
        disabled={missing.generating}
      >
        {missing.generating ? (
          <Loader2 className="mr-1 h-4 w-4 animate-spin" />
        ) : (
          <Key className="mr-1 h-4 w-4" />
        )}
        Generate Join Token
      </Button>
    </div>
  )
}

/**
 * How to add a worker node, for both ways a worker can reach this control
 * plane: over a private network they share, or over the internet through the
 * managed WireGuard mesh — which, when it is off, onboards instead of
 * disappearing.
 */
export function WorkerJoinGuide({
  token,
  missingToken,
}: {
  token: string | null
  missingToken?: MissingJoinToken
}) {
  const {
    data: mesh,
    isLoading,
    error,
    refetch,
    isFetching,
  } = useWireguardMesh()
  const [path, setPath] = useState<JoinPath | null>(null)
  const activePath = path ?? defaultJoinPath(mesh)
  const { url, configured } = joinUrl(mesh, window.location.origin)

  return (
    <div className="rounded-lg border bg-muted/30 p-4 space-y-4">
      <div>
        <p className="text-sm font-medium">How to add a worker node</p>
        <p className="text-sm text-muted-foreground">
          Pick how the new machine reaches this server.
        </p>
      </div>

      {!configured && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            No external URL configured
          </AlertTitle>
          <AlertDescription className="text-amber-600 dark:text-amber-300">
            These commands use this browser&apos;s address ({url}), which a
            worker on another machine may not reach.{' '}
            <Link to="/settings" className="font-medium underline">
              Set the external URL
            </Link>{' '}
            to the address workers can reach.
          </AlertDescription>
        </Alert>
      )}

      <Tabs
        value={activePath}
        onValueChange={(value) => setPath(value as JoinPath)}
      >
        <TabsList className="grid w-full grid-cols-2">
          <TabsTrigger value="private" className="gap-1.5">
            <Network className="h-4 w-4" />
            Same private network
          </TabsTrigger>
          <TabsTrigger value="internet" className="gap-1.5">
            <Globe className="h-4 w-4" />
            Over the internet
          </TabsTrigger>
        </TabsList>

        <TabsContent
          value="private"
          className="mt-4 space-y-3 text-sm text-muted-foreground"
        >
          <p>
            For machines in the same VPC, VLAN or datacenter network as this
            server — for example a Hetzner vSwitch or an AWS VPC.
          </p>
          <Step number={1} title="Install Temps on the worker">
            <CommandLine command={INSTALL_COMMAND} />
          </Step>
          <Step number={2} title="Join the cluster">
            {missingToken ? (
              <GenerateTokenFirst missing={missingToken} />
            ) : (
              <>
                <CommandLine command={joinCommand(url, token, 'private')} />
                <p className="mt-1 text-xs">
                  Replace <code>&lt;worker-private-ip&gt;</code> with the
                  worker&apos;s address on the network it shares with this
                  server (for example <code>10.0.0.5</code>).
                </p>
              </>
            )}
          </Step>
          <Step number={3} title="Start the agent, as root">
            <CommandLine command={AGENT_SERVICE_COMMAND} />
            <p className="mt-1 text-xs">
              Runs the worker as a systemd service that restarts on failure and
              at boot. {WITHOUT_SYSTEMD}
            </p>
          </Step>
        </TabsContent>

        <TabsContent
          value="internet"
          className="mt-4 space-y-3 text-sm text-muted-foreground"
        >
          {isLoading ? (
            <Skeletons />
          ) : error || !mesh ? (
            <QueryErrorAlert
              title="Could not read the WireGuard mesh state"
              error={error}
              onRetry={() => void refetch()}
              retrying={isFetching}
            />
          ) : (
            <InternetJoin
              mesh={mesh}
              url={url}
              token={token}
              missingToken={missingToken}
            />
          )}
        </TabsContent>
      </Tabs>

      <a
        href="https://temps.sh/docs/multi-node"
        target="_blank"
        rel="noopener noreferrer"
        className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline"
      >
        Full documentation
        <ExternalLink className="h-3 w-3" />
      </a>
    </div>
  )
}

function InternetJoin({
  mesh,
  url,
  token,
  missingToken,
}: {
  mesh: WireguardMeshStatusResponse
  url: string
  token: string | null
  missingToken?: MissingJoinToken
}) {
  if (mesh.state === 'disabled') return <MeshOnboarding mesh={mesh} />

  if (mesh.state === 'starting') {
    return (
      <Alert>
        <Loader2 className="h-4 w-4 animate-spin" />
        <AlertTitle>WireGuard mesh is starting</AlertTitle>
        <AlertDescription>{mesh.reason}</AlertDescription>
      </Alert>
    )
  }

  return (
    <InternetJoinReady
      mesh={mesh}
      url={url}
      token={token}
      missingToken={missingToken}
    />
  )
}

function InternetJoinReady({
  mesh,
  url,
  token,
  missingToken,
}: {
  mesh: WireguardMeshStatusResponse
  url: string
  token: string | null
  missingToken?: MissingJoinToken
}) {
  const [method, setMethod] = useState<InternetJoinMethod | null>(null)
  const active = method ?? defaultInternetMethod(mesh, url)
  const urlReachable = joinUrlReachableFromOutside(url)

  return (
    <>
      <p>
        Machines on another provider or network join over an encrypted WireGuard
        mesh ({mesh.cidr}). The control plane, the workers and their containers
        reach each other on private mesh addresses.
      </p>
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
        <Button
          type="button"
          size="sm"
          variant={active === 'ssh' ? 'default' : 'outline'}
          onClick={() => setMethod('ssh')}
        >
          <KeyRound className="mr-1 h-4 w-4" />
          Add it over SSH
        </Button>
        <Button
          type="button"
          size="sm"
          variant={active === 'pair' ? 'default' : 'outline'}
          onClick={() => setMethod('pair')}
        >
          <Link2 className="mr-1 h-4 w-4" />
          This server reaches the worker
        </Button>
        <Button
          type="button"
          size="sm"
          variant={active === 'url' ? 'default' : 'outline'}
          onClick={() => setMethod('url')}
        >
          <Globe className="mr-1 h-4 w-4" />
          The worker reaches this server
        </Button>
      </div>
      {active === 'ssh' ? (
        <SshEnrollNode mesh={mesh} />
      ) : active === 'pair' ? (
        <PairNode mesh={mesh} />
      ) : (
        <UrlJoin
          mesh={mesh}
          url={url}
          token={token}
          missingToken={missingToken}
          urlReachable={urlReachable}
        />
      )}
      <PendingPairings />
    </>
  )
}

function UrlJoin({
  mesh,
  url,
  token,
  missingToken,
  urlReachable,
}: {
  mesh: WireguardMeshStatusResponse
  url: string
  token: string | null
  missingToken?: MissingJoinToken
  urlReachable: boolean
}) {
  return (
    <>
      {!urlReachable && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            Workers elsewhere cannot reach {url}
          </AlertTitle>
          <AlertDescription className="text-amber-600 dark:text-amber-300">
            That address only works on this machine or its private network. Use{' '}
            <strong>This server reaches the worker</strong> instead, or{' '}
            <Link to="/settings" className="font-medium underline">
              set a public external URL
            </Link>
            .
          </AlertDescription>
        </Alert>
      )}
      {mesh.control_plane?.endpoint_is_private && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            Workers dial this server at a private address
          </AlertTitle>
          <AlertDescription className="text-amber-600 dark:text-amber-300">
            The mesh endpoint is {mesh.control_plane.endpoint}, which a machine
            on the internet cannot reach. Pair workers from here instead (This
            server reaches the worker), or start <code>temps serve</code> with{' '}
            <code>--private-address &lt;this server&apos;s public IP&gt;</code>.
          </AlertDescription>
        </Alert>
      )}
      <Step number={1} title="Open the mesh port">
        <p className="mt-1">
          Allow UDP <code>{mesh.listen_port}</code> in from the other nodes on
          this server and on the new worker (your provider&apos;s firewall and
          any host firewall).
        </p>
      </Step>
      <Step number={2} title="Install Temps on the worker">
        <CommandLine command={INSTALL_COMMAND} />
      </Step>
      <Step number={3} title="Join the cluster">
        {missingToken ? (
          <GenerateTokenFirst missing={missingToken} />
        ) : (
          <>
            <CommandLine command={joinCommand(url, token, 'internet')} />
            <p className="mt-1 text-xs">
              Replace <code>&lt;worker-public-ip&gt;</code> with the
              worker&apos;s public IP. It is only used until the worker is on
              the mesh.
            </p>
          </>
        )}
      </Step>
      <Step number={4} title="Start the agent, as root">
        <CommandLine command={AGENT_SERVICE_COMMAND} />
        <p className="mt-1 text-xs">
          The worker registers on the mesh and shows as Connected below within a
          minute. {WITHOUT_SYSTEMD} Behind NAT or a different public IP? Start
          it with{' '}
          <code>
            temps agent --wg-endpoint &lt;public-ip&gt;:{mesh.listen_port}
          </code>
          .
        </p>
      </Step>
    </>
  )
}

/** Pairings list, polled fast while any is in progress. */
function usePairings() {
  return useQuery({
    ...nodePairingListOptions(),
    refetchInterval: (query) =>
      pendingPairings(query.state.data?.pairings).length > 0 ? 3_000 : 30_000,
  })
}

/** The current time, updated every `intervalMs`, for relative times. */
function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), intervalMs)
    return () => window.clearInterval(timer)
  }, [intervalMs])
  return now
}

/** Pair a worker this server can reach (ADR 048 D2b): one command on it. */
function PairNode({ mesh }: { mesh: WireguardMeshStatusResponse }) {
  const queryClient = useQueryClient()
  const [address, setAddress] = useState('')
  const [name, setName] = useState('')
  const [created, setCreated] = useState<{
    command: string
    pairing: NodePairingResponse
  } | null>(null)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const create = useMutation({
    ...nodePairingCreateMutation(),
    onSuccess: async (data) => {
      setCreated({ command: data.join_command, pairing: data.pairing })
      await queryClient.invalidateQueries({
        queryKey: nodePairingListOptions().queryKey,
      })
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => create.mutate(variables)))
        return
      toast.error('Could not start pairing', {
        description: problemDetail(error, 'Check the address and try again.'),
      })
    },
  })
  const submit = () =>
    create.mutate({
      body: { address: address.trim(), name: name.trim() || null },
    })

  return (
    <div className="space-y-3">
      <p>
        For a worker this server can reach, when the worker cannot reach this
        server — for example this server runs on a laptop or behind NAT. This
        server dials the worker; no private key leaves either machine.
      </p>
      <Step number={1} title="Open the mesh port on the worker">
        <p className="mt-1">
          Allow UDP <code>{mesh.listen_port}</code> in on the worker (your
          provider&apos;s firewall and any host firewall). This server only
          needs to reach out.
        </p>
      </Step>
      <Step number={2} title="Install Temps on the worker">
        <CommandLine command={INSTALL_COMMAND} />
      </Step>
      <Step number={3} title="Create the pairing">
        <form
          className="mt-2 grid gap-2 sm:grid-cols-[1fr_1fr_auto] sm:items-end"
          onSubmit={(event) => {
            event.preventDefault()
            submit()
          }}
        >
          <div className="space-y-1">
            <Label htmlFor="pair-address">Worker public IP</Label>
            <Input
              id="pair-address"
              placeholder="203.0.113.10"
              value={address}
              onChange={(event) => setAddress(event.target.value)}
              required
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="pair-name">Name (optional)</Label>
            <Input
              id="pair-name"
              placeholder="worker-1"
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          <Button type="submit" disabled={!address.trim() || create.isPending}>
            {create.isPending && (
              <Loader2 className="mr-1 h-4 w-4 animate-spin" />
            )}
            Create pairing
          </Button>
        </form>
      </Step>
      {created && (
        <CreatedPairing
          command={created.command}
          created={created.pairing}
          onCreateAgain={() =>
            create.mutate({
              body: {
                address: address.trim() || created.pairing.node_endpoint,
                name: name.trim() || null,
              },
            })
          }
          creating={create.isPending}
        />
      )}
      {verificationDialog}
    </div>
  )
}

/**
 * Step 4 for the pairing just created: its one-time command while it can
 * still be used, with its live progress and expiry, and a way to start over
 * once it expired or was cancelled.
 */
function CreatedPairing({
  command,
  created,
  onCreateAgain,
  creating,
}: {
  command: string
  created: NodePairingResponse
  onCreateAgain: () => void
  creating: boolean
}) {
  const { data } = usePairings()
  const now = useNow(15_000)
  const listed = data?.pairings.find((pairing) => pairing.id === created.id)
  const pairing = pairingAsOf(listed ?? created, now)
  const { label, tone, hint } = pairingProgress(pairing)
  const badge = (
    <Badge variant="default" className={`${TONE_CLASSES[tone]} text-xs`}>
      {label}
    </Badge>
  )

  if (pairingEnded(pairing)) {
    return (
      <Step number={4} title={`Run the pairing command on ${pairing.name}`}>
        <Alert className="mt-1 border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            {pairing.status === 'cancelled'
              ? 'This pairing was cancelled: create a new one'
              : 'This pairing expired: create a new one'}
          </AlertTitle>
          <AlertDescription className="space-y-2 text-amber-600 dark:text-amber-300">
            <p>
              Its command no longer works.{' '}
              {pairing.status === 'expired' &&
                'The node did not answer before it expired. '}
              A new pairing for <code>{pairing.node_endpoint}</code> gives a new
              command.
            </p>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={onCreateAgain}
              disabled={creating}
            >
              {creating ? (
                <Loader2 className="mr-1 h-4 w-4 animate-spin" />
              ) : (
                <Link2 className="mr-1 h-4 w-4" />
              )}
              Create a new pairing
            </Button>
          </AlertDescription>
        </Alert>
      </Step>
    )
  }

  if (pairing.status === 'completed') {
    return (
      <Step number={4} title={`Start the agent on ${pairing.name}, as root`}>
        <div className="mt-1 flex items-center gap-2">
          <CheckCircle2 className="h-4 w-4 text-green-600" />
          <span>{pairing.name} is on the mesh.</span>
          {badge}
        </div>
        <CommandLine command={AGENT_SERVICE_COMMAND} />
      </Step>
    )
  }

  return (
    <Step number={4} title={`Run this on ${pairing.name}, as root`}>
      <CommandLine command={command} />
      <div className="mt-1 flex flex-wrap items-center gap-2 text-xs">
        {badge}
        <span>
          Expires <TimeAgo date={pairing.expires_at} />
        </span>
      </div>
      {hint && (
        <p className="mt-1 text-xs">
          <WithCode text={hint} />
        </p>
      )}
      <p className="mt-1 text-xs">
        It holds a one-time secret and is shown only now. It waits until this
        server reaches it at <code>{pairing.node_endpoint}</code>, brings the
        mesh up and registers. Then start the worker with{' '}
        <code>{AGENT_SERVICE_COMMAND}</code>.
      </p>
    </Step>
  )
}

/** Pairings in progress, with why a node has not been reached yet. */
function PendingPairings() {
  const queryClient = useQueryClient()
  const { data, error, refetch, isFetching } = usePairings()
  const cancel = useMutation({
    ...nodePairingCancelMutation(),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: nodePairingListOptions().queryKey,
      }),
    onError: (error) =>
      toast.error('Could not cancel the pairing', {
        description: problemDetail(error, 'Try again.'),
      }),
  })
  if (error) {
    return (
      <QueryErrorAlert
        title="Could not read the pairings in progress"
        error={error}
        onRetry={() => void refetch()}
        retrying={isFetching}
      />
    )
  }
  const pending = pendingPairings(data?.pairings)
  if (pending.length === 0) return null

  return (
    <div className="space-y-2 rounded-md border bg-background p-3">
      <p className="text-xs font-medium text-foreground">
        Pairings in progress
      </p>
      {pending.map((pairing) => {
        const { label, tone, hint } = pairingProgress(pairing)
        return (
          <div key={pairing.id} className="flex items-start gap-2 text-xs">
            <div className="min-w-0 flex-1">
              <p className="font-medium text-foreground">
                {pairing.name}{' '}
                <span className="font-mono text-muted-foreground">
                  {pairing.node_endpoint}
                </span>
              </p>
              {hint && (
                <p className="text-muted-foreground">
                  <WithCode text={hint} />
                </p>
              )}
            </div>
            <Badge
              variant="default"
              className={`${TONE_CLASSES[tone]} text-xs`}
            >
              {label}
            </Badge>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-6 w-6"
              title="Cancel this pairing"
              disabled={cancel.isPending}
              onClick={() =>
                cancel.mutate({ path: { pairing_id: pairing.id } })
              }
            >
              <X className="h-3.5 w-3.5" />
            </Button>
          </div>
        )
      })}
    </div>
  )
}

function MeshOnboarding({ mesh }: { mesh: WireguardMeshStatusResponse }) {
  const queryClient = useQueryClient()
  const [confirmOpen, setConfirmOpen] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const enable = useMutation({
    ...wireguardMeshEnableMutation(),
    onSuccess: async () => {
      toast.success('WireGuard mesh enabled', {
        description:
          'The control plane and workers move onto it within a minute.',
      })
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: wireguardMeshStatusGetOptions().queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: adminListNodesOptions().queryKey,
        }),
      ])
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => enable.mutate(variables)))
        return
      toast.error('Could not enable the WireGuard mesh', {
        description: problemDetail(
          error,
          'Check your permissions and try again.'
        ),
      })
    },
  })

  return (
    <div className="space-y-3">
      <div className="flex items-start gap-3 rounded-md border bg-background p-3">
        <ShieldCheck className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
        <div className="space-y-1">
          <p className="font-medium text-foreground">
            Turn on the WireGuard mesh to join machines over the internet
          </p>
          <p>
            A worker on another provider — or a box at home — only shares the
            internet with this server. The mesh gives every node a private
            address on an encrypted WireGuard tunnel, so a container on that
            worker reaches your database here as if both sat on one network.
          </p>
        </div>
      </div>
      <ul className="list-disc space-y-1 pl-5 text-xs">
        <li>
          Every node must accept UDP <code>{mesh.listen_port}</code> from the
          other nodes.
        </li>
        <li>Nodes run Linux with kernel WireGuard (5.6 or newer).</li>
        <li>
          Workers already on your private network keep using it. Once on, the
          mesh stays on.
        </li>
      </ul>

      {mesh.enable_blocker ? (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            This server cannot run the mesh yet
          </AlertTitle>
          <AlertDescription className="text-amber-600 dark:text-amber-300">
            {mesh.enable_blocker}
          </AlertDescription>
        </Alert>
      ) : (
        <Button
          onClick={() => setConfirmOpen(true)}
          disabled={!mesh.can_enable || enable.isPending}
        >
          {enable.isPending ? (
            <Loader2 className="h-4 w-4 animate-spin mr-1" />
          ) : (
            <ShieldCheck className="h-4 w-4 mr-1" />
          )}
          Enable WireGuard mesh
        </Button>
      )}

      <div>
        <p className="text-xs">Or on the control-plane host:</p>
        <CommandLine command={mesh.enable_command} />
      </div>

      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Enable the WireGuard mesh?</AlertDialogTitle>
            <AlertDialogDescription>
              Every node must accept UDP {mesh.listen_port} from the others.
              Cross-node traffic pauses for up to a minute while nodes switch
              over, and the mesh cannot be turned off again.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => enable.mutate({ body: {} })}>
              Enable mesh
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {verificationDialog}
    </div>
  )
}

/** A node's mesh connection for the node table. */
export function MeshConnectionBadge({
  connection,
  address,
  checks,
}: {
  connection: WireguardMeshNodeConnection
  address: string | null | undefined
  /** The control plane's checks for this node; problems get a fix popover. */
  checks?: WireguardMeshCheck[]
}) {
  const { label, tone, hint } = meshConnectionLabel(connection)
  const problems = meshProblems(checks)
  return (
    <div className="min-w-0" title={problems.length > 0 ? undefined : hint}>
      <Badge variant="default" className={`${TONE_CLASSES[tone]} text-xs`}>
        {label}
      </Badge>
      {address && (
        <span className="mt-0.5 block font-mono text-xs text-muted-foreground">
          {address}
        </span>
      )}
      {problems.length > 0 && (
        <Popover>
          <PopoverTrigger asChild>
            <button
              type="button"
              className="mt-0.5 block text-xs text-amber-700 underline-offset-2 hover:underline dark:text-amber-400"
              onClick={(event) => event.stopPropagation()}
            >
              {problems.length === 1
                ? 'What to fix'
                : `${problems.length} things to fix`}
            </button>
          </PopoverTrigger>
          <PopoverContent
            className="w-[calc(100vw-2rem)] space-y-3 text-xs sm:w-96"
            onClick={(event) => event.stopPropagation()}
          >
            {problems.map((check) => (
              <div key={check.label}>
                <p className="font-medium text-foreground">
                  {check.label}: <WithCode text={check.detail} />
                </p>
                {check.fix && (
                  <p className="mt-0.5 text-muted-foreground">
                    <WithCode text={check.fix} />
                  </p>
                )}
              </div>
            ))}
            <p className="border-t pt-2 text-muted-foreground">
              This is what the control plane can see. For the node&apos;s own
              view, run <code>temps doctor mesh</code> on it.
            </p>
          </PopoverContent>
        </Popover>
      )}
    </div>
  )
}
