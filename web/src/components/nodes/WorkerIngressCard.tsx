// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ScrollRegion } from '@/components/ui/scroll-region'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  CheckCircle2,
  ChevronRight,
  Globe,
  Loader2,
  Radar,
  XCircle,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  adminCheckNodePublicIngressMutation,
  adminGetNodeOptions,
  adminGetNodePublicIngressSettingsOptions,
  adminListNodesOptions,
  adminUpdateNodePublicIngressSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import type {
  NodeInfoResponse,
  PublicIngressCheck,
  PublicIngressSettings,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { WorkerIngressSummary } from '@/components/nodes/WorkerIngressSummary'
import { WorkerTunnels } from '@/components/nodes/WorkerTunnels'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  ALLOW_INGRESS_COMMANDS,
  type AddressScope,
  addressScope,
  scopeLabel,
  workerIngressStatus,
} from '@/lib/worker-ingress'

const AUTOMATIC = '__automatic__'

const port = z
  .string()
  .trim()
  .regex(/^\d+$/, 'Enter a port number.')
  .refine(
    (p) => Number(p) >= 1 && Number(p) <= 65535,
    'Use a port from 1 to 65535.'
  )

/** The open-ports settings, checked as the API checks them. */
const draftSchema = z
  .object({
    enabled: z.boolean(),
    listen: z.string(),
    publicAddress: z
      .string()
      .trim()
      .refine(
        (a) => !a || addressScope(a) !== undefined,
        'Enter an IP address: the one your DNS A or AAAA record points at.'
      )
      .refine((a) => {
        const scope = a ? addressScope(a) : undefined
        return scope === undefined || scope === 'public'
      }, 'Visitors on the internet cannot reach this address. Use a public IP.'),
    httpPort: port,
    httpsPort: port,
  })
  .refine((d) => d.httpPort.trim() !== d.httpsPort.trim(), {
    path: ['httpsPort'],
    message: 'HTTP and HTTPS need two different ports.',
  })

type Draft = z.infer<typeof draftSchema>

const EMPTY_DRAFT: Draft = {
  enabled: false,
  listen: AUTOMATIC,
  publicAddress: '',
  httpPort: '80',
  httpsPort: '443',
}

function draftFrom(settings: PublicIngressSettings): Draft {
  return {
    enabled: settings.enabled,
    listen: settings.listen_address ?? AUTOMATIC,
    publicAddress: settings.public_address ?? '',
    httpPort: String(settings.http_port),
    httpsPort: String(settings.https_port),
  }
}

export function WorkerIngressCard({ node }: { node: NodeInfoResponse }) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const settingsQuery = useQuery({
    ...adminGetNodePublicIngressSettingsOptions({
      path: { node_id: node.id },
    }),
    // The agent reports what it bound within seconds of a change.
    refetchInterval: 10_000,
  })
  const settings = settingsQuery.data
  const reported = useMemo(
    () => (settings ? draftFrom(settings) : undefined),
    [settings]
  )
  const form = useForm<Draft>({
    resolver: zodResolver(draftSchema),
    mode: 'onChange',
    defaultValues: EMPTY_DRAFT,
    // Follows what the worker reports, without overwriting the operator's
    // unsaved edits.
    values: reported,
    resetOptions: { keepDirtyValues: true },
  })
  const watched = useWatch({ control: form.control })
  const draft: Draft | null = settings ? { ...EMPTY_DRAFT, ...watched } : null
  const [check, setCheck] = useState<PublicIngressCheck | null>(null)
  // Shared with WorkerTunnels' query, so no extra request.
  const connectorsQuery = useQuery(allTunnelsOptions())
  const tunnelHere = (connectorsQuery.data ?? []).some((c) =>
    c.nodes.some((n) => n.node_id === node.id)
  )
  // `null` until the operator opens or closes it: then the default applies.
  const [directOpen, setDirectOpen] = useState<boolean | null>(null)
  // The page reports how traffic gets in; changing it is a deliberate step.
  const [editing, setEditing] = useState(false)
  const [portsOpen, setPortsOpen] = useState(false)

  const refresh = async () => {
    await Promise.all([
      queryClient.invalidateQueries({
        queryKey: adminGetNodePublicIngressSettingsOptions({
          path: { node_id: node.id },
        }).queryKey,
      }),
      queryClient.invalidateQueries({
        queryKey: adminGetNodeOptions({ path: { node_id: node.id } }).queryKey,
      }),
      queryClient.invalidateQueries({
        queryKey: adminListNodesOptions().queryKey,
      }),
    ])
  }

  const update = useMutation({
    ...adminUpdateNodePublicIngressSettingsMutation(),
    onSuccess: async (saved) => {
      toast.success(
        saved.enabled
          ? 'Saved. The worker applies it within a few seconds.'
          : 'Public ingress turned off'
      )
      form.reset(draftFrom(saved))
      setCheck(null)
      await refresh()
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => update.mutate(variables)))
        return
      toast.error('Could not save public ingress', {
        description: problemDetail(
          error,
          'Check your permissions and try again.'
        ),
      })
    },
  })

  const runCheck = useMutation({
    ...adminCheckNodePublicIngressMutation(),
    onSuccess: async (result) => {
      setCheck(result)
      await refresh()
    },
    onError: (error) =>
      toast.error('Could not run the check', {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })

  const save = (next: Draft) =>
    update.mutate({
      path: { node_id: node.id },
      body: {
        enabled: next.enabled,
        listen_address: next.listen === AUTOMATIC ? null : next.listen,
        public_address: next.publicAddress.trim() || null,
        http_port: Number(next.httpPort) || 80,
        https_port: Number(next.httpsPort) || 443,
      },
    })

  const state = workerIngressStatus(node)
  const automatic = settings?.interfaces.find((i) => i.default_route)
  const listenIp =
    draft && draft.listen !== AUTOMATIC ? draft.listen : automatic?.address
  const listenScope = listenIp ? addressScope(listenIp) : undefined
  const publicIp = draft?.publicAddress.trim() || undefined
  const publicScope = publicIp ? addressScope(publicIp) : undefined
  const dirty = form.formState.isDirty
  const { errors } = form.formState
  // A port the API would refuse opens the ports so the reason is visible.
  const portError = !!errors.httpPort || !!errors.httpsPort
  const saveDraft = form.handleSubmit((values) =>
    save({ ...values, enabled: true })
  )
  // The worker decides what may change here (ADR 058 D7). Agents that
  // predate modes report none and keep the old, console-managed behaviour.
  const mode = settings?.mode ?? null
  const workerOff = mode === 'off'
  const listenLocked = mode === 'off' || mode === 'fixed'
  const shownCheck = check ?? settings?.last_check ?? null
  // Tunnels expose a worker as much as open ports do, so they need the
  // same permission (ADR 058 D8).
  const tunnelsAllowed = mode === null || mode === 'delegated'
  // Without an address the internet can open connections to, a tunnel is
  // the only way in: lead with it.
  const tunnelFirst = listenScope !== 'public' && publicScope !== 'public'
  // What the server has saved, not the draft: the switch turns on only once
  // the save succeeds, and stays off if the settings are refused.
  const enabled = settings?.enabled ?? node.public_ingress_enabled
  // With a tunnel already carrying this worker, open ports are a second,
  // optional way in: keep their settings folded away unless asked for.
  const showDirectSettings = directOpen ?? (enabled && !tunnelHere)

  const tunnels = (
    <section className="space-y-3" aria-labelledby={`tunnels-${node.id}`}>
      <div className="space-y-0.5">
        <h3 id={`tunnels-${node.id}`} className="text-sm font-medium">
          Through a tunnel
        </h3>
        {!tunnelHere && (
          <p className="text-sm text-muted-foreground">
            The worker connects out to a tunnel provider, and visitors come in
            through it. No public IP, open port or router change.
          </p>
        )}
      </div>
      {mode === 'fixed' ? (
        <p className="text-sm text-muted-foreground">
          This worker manages its own ingress, so Temps can&apos;t add a tunnel
          to it. Hand ingress to this page with{' '}
          <code>{ALLOW_INGRESS_COMMANDS.delegated}</code> on the worker.
        </p>
      ) : (
        <WorkerTunnels
          node={node}
          allowed={tunnelsAllowed}
          recommended={tunnelFirst}
        />
      )}
    </section>
  )

  return (
    <Card className="shadow-none" id="public-ingress">
      <CardContent className="space-y-4 p-4">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Globe className="h-4 w-4" /> How visitors reach this worker
            </div>
            {editing && (
              <p className="text-sm text-muted-foreground">
                Take application traffic on this worker and forward it to
                containers on any worker in the cluster. Apps&apos; hostnames
                and certificates are under{' '}
                <Link className="underline" to="/domains">
                  Domains
                </Link>
                .
              </p>
            )}
          </div>
          {settings && (
            <Button
              size="sm"
              variant={editing ? 'default' : 'outline'}
              onClick={() => setEditing(!editing)}
            >
              {editing ? 'Done' : 'Change'}
            </Button>
          )}
        </div>

        {settingsQuery.isLoading && (
          <div className="space-y-3" aria-busy="true">
            {[0, 1, 2].map((row) => (
              <div
                key={row}
                className="grid gap-1 sm:grid-cols-[10rem_1fr] sm:gap-4"
              >
                <Skeleton className="h-4 w-24" />
                <Skeleton className="h-4 w-full max-w-sm" />
              </div>
            ))}
          </div>
        )}

        {settings && !editing && (
          <WorkerIngressSummary
            node={node}
            settings={settings}
            onChange={() => setEditing(true)}
          />
        )}

        {settings && draft && editing && (
          <div className="space-y-4">
            {!settings.agent_reports && (
              <Alert>
                <AlertDescription>
                  This worker&apos;s agent hasn&apos;t reported its network
                  addresses. Upgrade Temps on it to manage its listener from
                  here; until then it listens only where its{' '}
                  <code>--public-ingress-address</code> flag says.
                </AlertDescription>
              </Alert>
            )}
            {workerOff && (
              <Alert>
                <AlertDescription className="space-y-2">
                  <p>
                    This worker doesn&apos;t take public traffic, and only the
                    worker can change that. Run this as root on it:
                  </p>
                  <WorkerCommand
                    command={ALLOW_INGRESS_COMMANDS.delegated}
                    hint="Recommended: then pick a tunnel or open ports here."
                  />
                  <p className="text-xs text-muted-foreground">
                    Or keep the choice on the worker, listening on its main
                    address on ports 80 and 443 (no tunnels):
                  </p>
                  <WorkerCommand command={ALLOW_INGRESS_COMMANDS.fixed} />
                </AlertDescription>
              </Alert>
            )}

            {!workerOff && tunnelFirst && tunnels}

            {!workerOff && (
              <section
                className="space-y-4 border-t pt-4"
                aria-labelledby={`direct-${node.id}`}
              >
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0 space-y-0.5">
                    <div className="flex items-center gap-2">
                      <h3
                        id={`direct-${node.id}`}
                        className="text-sm font-medium"
                      >
                        Directly, on open ports
                      </h3>
                      <Badge variant="outline">{state.label}</Badge>
                    </div>
                    {!enabled ? (
                      <p className="text-sm text-muted-foreground">
                        {tunnelHere
                          ? 'Not needed while the tunnel carries this worker.'
                          : 'Listen on ports 80 and 443, with DNS pointing at a public IP. Needs a public IP, or a router you can forward ports on.'}
                      </p>
                    ) : (
                      !showDirectSettings && (
                        <p className="text-sm" role="status">
                          {state.description}
                          {tunnelHere &&
                            node.public_ingress_last_error &&
                            ' The tunnel already carries this worker, so you can turn this off.'}
                        </p>
                      )
                    )}
                  </div>
                  <Switch
                    aria-label="Take traffic on open ports"
                    checked={enabled}
                    disabled={!draft || update.isPending}
                    onCheckedChange={(on) => {
                      if (!draft || !settings) return
                      if (on) {
                        // Turning on saves the settings below: check them.
                        // Open, so a refused field shows its reason.
                        setDirectOpen(true)
                        void saveDraft()
                      } else {
                        // Turning off keeps the saved settings for next time.
                        save({ ...draftFrom(settings), enabled: false })
                      }
                    }}
                  />
                </div>
                <Collapsible
                  open={showDirectSettings}
                  onOpenChange={setDirectOpen}
                >
                  <CollapsibleTrigger className="group flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
                    <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
                    Settings
                  </CollapsibleTrigger>
                  <CollapsibleContent className="space-y-4 pt-3">
                    {mode === 'fixed' && (
                      <Alert>
                        <AlertDescription className="space-y-2">
                          <p>
                            This worker chose its own listen address and ports
                            {settings.listening_http
                              ? ` (${settings.listening_http} and ${settings.listening_https})`
                              : ''}
                            . Change them on the worker, or hand them to this
                            page:
                          </p>
                          <WorkerCommand command="temps agent ingress fixed --listen <address|auto> --http-port 80 --https-port 443" />
                          <WorkerCommand
                            command={ALLOW_INGRESS_COMMANDS.delegated}
                          />
                        </AlertDescription>
                      </Alert>
                    )}

                    <Form {...form}>
                      <form onSubmit={saveDraft} className="space-y-4">
                        <div className="grid gap-4 md:grid-cols-2">
                          <FormField
                            control={form.control}
                            name="listen"
                            render={({ field }) => (
                              <FormItem>
                                <FormLabel>Listen on</FormLabel>
                                <Select
                                  value={field.value}
                                  onValueChange={field.onChange}
                                  disabled={listenLocked}
                                >
                                  <FormControl>
                                    <SelectTrigger>
                                      <SelectValue />
                                    </SelectTrigger>
                                  </FormControl>
                                  <SelectContent>
                                    <SelectItem value={AUTOMATIC}>
                                      Automatic
                                      {automatic
                                        ? ` (${automatic.name} · ${automatic.address})`
                                        : ''}
                                    </SelectItem>
                                    {settings.interfaces.map((i) => (
                                      <SelectItem
                                        key={`${i.name}-${i.address}`}
                                        value={i.address}
                                      >
                                        {i.name} · {i.address} ·{' '}
                                        {scopeLabel(i.scope)}
                                      </SelectItem>
                                    ))}
                                    {field.value !== AUTOMATIC &&
                                      !settings.interfaces.some(
                                        (i) => i.address === field.value
                                      ) && (
                                        <SelectItem value={field.value}>
                                          {field.value} (not reported by the
                                          worker)
                                        </SelectItem>
                                      )}
                                  </SelectContent>
                                </Select>
                                <FormDescription className="text-xs">
                                  An address of this machine. Automatic follows
                                  the default route.
                                </FormDescription>
                                <FormMessage />
                              </FormItem>
                            )}
                          />
                          <FormField
                            control={form.control}
                            name="publicAddress"
                            render={({ field }) => (
                              <FormItem>
                                <FormLabel>Public address</FormLabel>
                                <FormControl>
                                  <Input
                                    placeholder={
                                      listenScope === 'public' && listenIp
                                        ? `${listenIp} (same as listen address)`
                                        : 'Your router’s public IP, e.g. 203.0.113.7'
                                    }
                                    {...field}
                                  />
                                </FormControl>
                                <FormDescription className="text-xs">
                                  The IP your DNS A record points at.
                                </FormDescription>
                                <FormMessage />
                              </FormItem>
                            )}
                          />
                        </div>

                        <NetworkHint
                          listenIp={listenIp}
                          listenScope={listenScope}
                          publicIp={publicIp}
                          publicScope={publicScope}
                          httpPort={draft.httpPort}
                          httpsPort={draft.httpsPort}
                        />

                        <Collapsible
                          open={portsOpen || portError}
                          onOpenChange={setPortsOpen}
                        >
                          <CollapsibleTrigger className="group flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
                            <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
                            Local ports
                          </CollapsibleTrigger>
                          <CollapsibleContent className="grid gap-4 pt-3 md:grid-cols-2">
                            <FormField
                              control={form.control}
                              name="httpPort"
                              render={({ field }) => (
                                <FormItem>
                                  <FormLabel>HTTP port</FormLabel>
                                  <FormControl>
                                    <Input
                                      inputMode="numeric"
                                      disabled={listenLocked}
                                      {...field}
                                    />
                                  </FormControl>
                                  <FormMessage />
                                </FormItem>
                              )}
                            />
                            <FormField
                              control={form.control}
                              name="httpsPort"
                              render={({ field }) => (
                                <FormItem>
                                  <FormLabel>HTTPS port</FormLabel>
                                  <FormControl>
                                    <Input
                                      inputMode="numeric"
                                      disabled={listenLocked}
                                      {...field}
                                    />
                                  </FormControl>
                                  <FormMessage />
                                </FormItem>
                              )}
                            />
                            <p className="text-xs text-muted-foreground md:col-span-2">
                              Change these only when port 80 or 443 is taken on
                              this machine, or the agent can&apos;t bind ports
                              below 1024. Visitors still use 80 and 443: forward
                              them to these ports.
                            </p>
                          </CollapsibleContent>
                        </Collapsible>

                        <div className="flex flex-wrap items-center gap-2">
                          <Button
                            type="submit"
                            size="sm"
                            disabled={!dirty || update.isPending}
                          >
                            {update.isPending && (
                              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                            )}
                            {settings.enabled ? 'Save' : 'Save and turn on'}
                          </Button>
                          {dirty && (
                            <Button
                              type="button"
                              size="sm"
                              variant="ghost"
                              onClick={() => form.reset(draftFrom(settings))}
                            >
                              Discard
                            </Button>
                          )}
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            disabled={
                              !settings.enabled || runCheck.isPending || dirty
                            }
                            onClick={() =>
                              runCheck.mutate({ path: { node_id: node.id } })
                            }
                          >
                            {runCheck.isPending ? (
                              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                            ) : (
                              <Radar className="mr-2 h-4 w-4" />
                            )}
                            Check reachability
                          </Button>
                        </div>
                      </form>
                    </Form>

                    {settings.enabled && <ListenerStatus settings={settings} />}
                    {shownCheck && <CheckResult check={shownCheck} />}
                  </CollapsibleContent>
                </Collapsible>
              </section>
            )}

            {!workerOff && !tunnelFirst && (
              <div className="border-t pt-4">{tunnels}</div>
            )}
          </div>
        )}

        {node.public_ingress_enabled &&
          ((node.public_ingress_unsupported_route_count ?? 0) > 0 ||
            node.public_ingress_unsupported_reasons.length > 0) && (
            <Alert>
              <AlertDescription className="space-y-2">
                <p>
                  {node.public_ingress_unsupported_route_count ?? 0} application
                  routes are unavailable through this worker.
                </p>
                {node.public_ingress_unsupported_reasons.length > 0 && (
                  <ul className="list-disc space-y-1 pl-5">
                    {node.public_ingress_unsupported_reasons.map((reason) => (
                      <li key={reason}>{reason}</li>
                    ))}
                  </ul>
                )}
              </AlertDescription>
            </Alert>
          )}

        {editing && (
          <Collapsible>
            <CollapsibleTrigger className="group flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
              <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
              Limits
            </CollapsibleTrigger>
            <CollapsibleContent className="pt-3">
              <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
                <li>
                  Supports container applications with domain-specific TLS
                  certificates. Wildcard-only certificates are not distributed
                  to workers. Routes that require static-file serving,
                  redirects, wake-up, or security policies that workers cannot
                  enforce stay unavailable through worker ingress.
                </li>
                <li>
                  A single ingress worker is a single entry point; use a
                  health-checking load balancer for ingress failover.
                </li>
                <li>
                  Workers retain their last authorized routing configuration for
                  up to five minutes without the control plane. After that, new
                  requests fail closed until synchronization recovers.
                </li>
                <li>
                  Before turning ingress off, move DNS or load-balancer traffic
                  to another ingress worker: turning it off releases the ports
                  at once.
                </li>
              </ul>
            </CollapsibleContent>
          </Collapsible>
        )}
      </CardContent>
      {verificationDialog}
    </Card>
  )
}

/** What the operator has to do on their network, from the two addresses. */
function NetworkHint({
  listenIp,
  listenScope,
  publicIp,
  publicScope,
  httpPort,
  httpsPort,
}: {
  listenIp?: string
  listenScope?: AddressScope
  publicIp?: string
  publicScope?: AddressScope
  httpPort: string
  httpsPort: string
}) {
  if (!listenIp) return null
  const http = `${listenIp}:${httpPort || 80}`
  const https = `${listenIp}:${httpsPort || 443}`
  if (publicScope === 'cgnat' || (!publicIp && listenScope === 'cgnat')) {
    return (
      <Alert variant="destructive">
        <AlertDescription>
          {publicIp ?? listenIp} is behind carrier-grade NAT (100.64.0.0/10):
          the internet can&apos;t open connections to it and no port forward
          helps. Use a tunnel for this worker instead.
        </AlertDescription>
      </Alert>
    )
  }
  if (publicScope === 'private' || publicScope === 'local') {
    return (
      <Alert variant="destructive">
        <AlertDescription>
          {publicIp} is a private address, so visitors on the internet
          can&apos;t reach it. Enter the public IP of the router in front of
          this worker.
        </AlertDescription>
      </Alert>
    )
  }
  if (listenScope === 'private' && !publicIp) {
    return (
      <Alert>
        <AlertDescription>
          {listenIp} is a private address: this worker is behind a router. Enter
          the router&apos;s public IP above, and on the router forward TCP 80 to{' '}
          {http} and TCP 443 to {https}. No public IP? Use a tunnel instead.
        </AlertDescription>
      </Alert>
    )
  }
  if (listenScope === 'private' && publicScope === 'public') {
    return (
      <Alert>
        <AlertDescription>
          Behind NAT. On the router that owns {publicIp}, forward TCP 80 to{' '}
          {http} and TCP 443 to {https}. Then point DNS at {publicIp}.
        </AlertDescription>
      </Alert>
    )
  }
  if (listenScope === 'public') {
    const target = publicIp ?? listenIp
    return (
      <p className="text-sm text-muted-foreground">
        Visitors reach {target} directly: allow inbound TCP 80 and 443 in this
        machine&apos;s firewall and your provider&apos;s security group, then
        point DNS at {target}.
      </p>
    )
  }
  return null
}

function ListenerStatus({ settings }: { settings: PublicIngressSettings }) {
  if (settings.listening_http && settings.listening_https) {
    return (
      <p className="flex items-center gap-2 text-sm">
        <CheckCircle2 className="h-4 w-4 text-green-600" />
        Listening on {settings.listening_http} and {settings.listening_https}
      </p>
    )
  }
  const reason =
    settings.listener_error ??
    (settings.agent_reports
      ? 'Waiting for the worker to bind its listeners…'
      : null)
  if (!reason) return null
  return (
    <p className="flex items-start gap-2 text-sm">
      <XCircle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
      <span>{reason}</span>
    </p>
  )
}

function CheckResult({ check }: { check: PublicIngressCheck }) {
  return (
    <Alert variant={check.ok ? 'default' : 'destructive'}>
      <AlertDescription className="space-y-2">
        <p className="font-medium">{check.summary}</p>
        {check.steps.length > 0 && (
          <ul className="space-y-1">
            {check.steps.map((step) => (
              <li key={step.step} className="flex items-start gap-2 text-sm">
                {step.ok ? (
                  <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-green-600" />
                ) : (
                  <XCircle className="mt-0.5 h-4 w-4 shrink-0" />
                )}
                <span>{step.message}</span>
              </li>
            ))}
          </ul>
        )}
      </AlertDescription>
    </Alert>
  )
}

function WorkerCommand({ command, hint }: { command: string; hint?: string }) {
  return (
    <div className="space-y-1">
      <div className="flex items-center gap-2 rounded-md bg-muted px-3 py-1.5 font-mono text-xs">
        <ScrollRegion
          className="min-w-0 flex-1 overflow-x-auto"
          regionLabel="Command, scrollable"
        >
          <code className="whitespace-pre">{command}</code>
        </ScrollRegion>
        <CopyButton minimal className="h-6 w-6 shrink-0" value={command} />
      </div>
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  )
}
