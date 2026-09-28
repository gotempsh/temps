// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ExternalLink,
  Globe,
  Loader2,
  Network,
  ShieldCheck,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  adminListNodesOptions,
  wireguardMeshEnableMutation,
  wireguardMeshStatusGetOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  defaultJoinPath,
  joinCommand,
  joinUrl,
  meshConnectionLabel,
  type JoinPath,
} from '@/lib/wireguard-mesh'

const INSTALL_COMMAND = 'curl -fsSL https://temps.sh/install.sh | bash'

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
 * How to add a worker node, for both ways a worker can reach this control
 * plane: over a private network they share, or over the internet through the
 * managed WireGuard mesh — which, when it is off, onboards instead of
 * disappearing.
 */
export function WorkerJoinGuide({ token }: { token: string | null }) {
  const { data: mesh, isLoading, error } = useWireguardMesh()
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
            <CommandLine command={joinCommand(url, token, 'private')} />
            <p className="mt-1 text-xs">
              Replace <code>&lt;worker-private-ip&gt;</code> with the
              worker&apos;s address on the network it shares with this server
              (for example <code>10.0.0.5</code>).
            </p>
          </Step>
          <Step number={3} title="Start the agent">
            <CommandLine command="temps agent" />
            <p className="mt-1 text-xs">
              Reads the config saved by <code>temps join</code> and starts the
              worker with heartbeats.
            </p>
          </Step>
        </TabsContent>

        <TabsContent
          value="internet"
          className="mt-4 space-y-3 text-sm text-muted-foreground"
        >
          {isLoading ? (
            <div className="flex items-center gap-2">
              <Loader2 className="h-4 w-4 animate-spin" />
              Checking the WireGuard mesh...
            </div>
          ) : error || !mesh ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>Could not read the WireGuard mesh state</AlertTitle>
              <AlertDescription>
                {problemDetail(error, 'Reload the page to try again.')}
              </AlertDescription>
            </Alert>
          ) : (
            <InternetJoin mesh={mesh} url={url} token={token} />
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
}: {
  mesh: WireguardMeshStatusResponse
  url: string
  token: string | null
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
    <>
      <p>
        Machines on another provider or network join over an encrypted WireGuard
        mesh ({mesh.cidr}). The control plane, the workers and their containers
        reach each other on private mesh addresses.
      </p>
      {mesh.control_plane?.endpoint_is_private && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            Workers dial this server at a private address
          </AlertTitle>
          <AlertDescription className="text-amber-600 dark:text-amber-300">
            The mesh endpoint is {mesh.control_plane.endpoint}, which a machine
            on the internet cannot reach. Start <code>temps serve</code> with{' '}
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
        <CommandLine command={joinCommand(url, token, 'internet')} />
        <p className="mt-1 text-xs">
          Replace <code>&lt;worker-public-ip&gt;</code> with the worker&apos;s
          public IP. It is only used until the worker is on the mesh.
        </p>
      </Step>
      <Step number={4} title="Start the agent">
        <CommandLine command="temps agent" />
        <p className="mt-1 text-xs">
          The worker registers on the mesh and shows as Connected below within a
          minute. Behind NAT or a different public IP? Start it with{' '}
          <code>
            temps agent --wg-endpoint &lt;public-ip&gt;:{mesh.listen_port}
          </code>
          .
        </p>
      </Step>
    </>
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

const TONE_CLASSES = {
  ok: 'bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20',
  warn: 'bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/20',
  error: 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/20',
  muted: 'bg-gray-500/15 text-gray-700 dark:text-gray-400 border-gray-500/20',
} as const

/** A node's mesh connection for the node table. */
export function MeshConnectionBadge({
  connection,
  address,
}: {
  connection: WireguardMeshNodeConnection
  address: string | null | undefined
}) {
  const { label, tone, hint } = meshConnectionLabel(connection)
  return (
    <div className="min-w-0" title={hint}>
      <Badge variant="default" className={`${TONE_CLASSES[tone]} text-xs`}>
        {label}
      </Badge>
      {address && (
        <span className="mt-0.5 block font-mono text-xs text-muted-foreground">
          {address}
        </span>
      )}
    </div>
  )
}
