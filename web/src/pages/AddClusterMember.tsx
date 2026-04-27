import { adminListNodesOptions, getServiceOptions } from '@/api/client/@tanstack/react-query.gen'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Circle,
  Loader2,
  Server,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

// Mirror of `member_provisioning_step` in temps-providers/src/services.rs.
// The backend writes one of these values into
// `service_members.provisioning_step` after each phase boundary; we
// poll the row at 1s and render the timeline.
const PROVISIONING_STEPS: { id: string; label: string; description: string }[] =
  [
    {
      id: 'inserting_row',
      label: 'Reserving slot',
      description:
        'Allocating the next ordinal and inserting a placeholder row in service_members.',
    },
    {
      id: 'provisioning_container',
      label: 'Provisioning container',
      description:
        'Pulling the postgres-ha image and starting the new replica on the chosen node.',
    },
    {
      id: 'registering_dns',
      label: 'Registering DNS record',
      description:
        'Publishing the per-member A record so other cluster members can resolve it.',
    },
    {
      id: 'done',
      label: 'Joining cluster',
      description:
        'pg_autoctl registers with the monitor; reconciler refreshes role-aliased VIPs on its next tick (≤30s).',
    },
  ]

type WireMember = {
  id: number
  role: string
  node_id?: number | null
  container_name: string
  hostname?: string | null
  port?: number | null
  status: string
  ordinal: number
  compute_ip?: string | null
  provisioning_step?: string | null
  provisioning_error?: string | null
}

function stepStatus(
  member: WireMember | undefined,
  stepId: string
): 'done' | 'active' | 'pending' | 'failed' {
  if (!member) return 'pending'
  if (member.provisioning_step === 'failed') {
    // Show every step before the failure as done, the failed step as
    // failed, and everything after as pending. The backend doesn't
    // record *which* step failed — we infer it from `status='failed'`
    // and surface the error string verbatim.
    return 'failed'
  }
  const order = PROVISIONING_STEPS.map((s) => s.id)
  const currentIdx = order.indexOf(member.provisioning_step ?? '')
  const stepIdx = order.indexOf(stepId)
  if (currentIdx < 0) return 'pending'
  if (stepIdx < currentIdx) return 'done'
  if (stepIdx === currentIdx) return member.status === 'running' && stepId === 'done' ? 'done' : 'active'
  return 'pending'
}

export function AddClusterMember() {
  const { id } = useParams<{ id: string }>()
  const serviceIdNum = id ? parseInt(id, 10) : NaN
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Add Cluster Member')

  // Form state — kept simple because the only knob today is "which node".
  // Role is fixed at "replica" (monitor is singleton, primary is elected).
  const [nodeId, setNodeId] = useState<string>('control-plane')

  // The newly-created member's id, set after POST returns 202. The
  // polling query is enabled only once we have it.
  const [memberId, setMemberId] = useState<number | null>(null)

  const serviceQuery = useQuery({
    ...getServiceOptions({ path: { id: serviceIdNum } }),
    enabled: Number.isFinite(serviceIdNum),
  })

  const nodesQuery = useQuery({
    ...adminListNodesOptions(),
    enabled: Number.isFinite(serviceIdNum),
  })

  const activeNodes = useMemo(
    () =>
      (nodesQuery.data?.nodes ?? []).filter(
        (n: NodeInfoResponse) => n.status === 'active'
      ),
    [nodesQuery.data]
  )

  // Poll the member row every 1s until it transitions to a terminal
  // step (`done` or `failed`). The endpoint mirrors the existing
  // service-detail polling pattern.
  const memberQuery = useQuery<WireMember>({
    queryKey: ['cluster-member', serviceIdNum, memberId],
    queryFn: async () => {
      const response = await fetch(
        `/api/external-services/${serviceIdNum}/members/${memberId}`,
        { credentials: 'include' }
      )
      if (!response.ok) {
        const err = await response.json().catch(() => ({}))
        throw new Error(err.detail || 'Failed to load member')
      }
      return response.json()
    },
    enabled: Number.isFinite(serviceIdNum) && memberId !== null,
    refetchInterval: (query) => {
      const step = query.state.data?.provisioning_step
      const isTerminal = step === 'done' || step === 'failed'
      return isTerminal ? false : 1000
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Storage', href: '/storage' },
      {
        label: serviceQuery.data?.service?.name ?? `Service ${id}`,
        href: `/storage/${id}`,
      },
      { label: 'Add Cluster Member' },
    ])
  }, [id, serviceQuery.data?.service?.name, setBreadcrumbs])

  const addMember = useMutation({
    mutationFn: async () => {
      const response = await fetch(
        `/api/external-services/${serviceIdNum}/members`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          credentials: 'include',
          body: JSON.stringify({
            role: 'replica',
            node_id: nodeId === 'control-plane' ? null : Number(nodeId),
          }),
        }
      )
      if (!response.ok) {
        const err = await response.json().catch(() => ({}))
        throw new Error(err.detail || 'Failed to start provisioning')
      }
      return (await response.json()) as WireMember
    },
    onSuccess: (member) => {
      setMemberId(member.id)
      toast.success('Provisioning started', {
        description: `Member ${member.container_name} is being created.`,
      })
    },
    onError: (error: Error) => {
      toast.error('Failed to start provisioning', {
        description: error.message,
      })
    },
  })

  const member = memberQuery.data
  const isFailed = member?.provisioning_step === 'failed'
  const isDone = member?.provisioning_step === 'done'

  return (
    <div className="container max-w-3xl mx-auto py-6 space-y-4">
      <div>
        <Button
          variant="ghost"
          size="sm"
          asChild
          className="text-muted-foreground"
        >
          <Link to={`/storage/${id}`}>
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back to service
          </Link>
        </Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Add Cluster Member</CardTitle>
          <CardDescription>
            Provision a new replica and register it with the existing
            pg_auto_failover monitor. The role reconciler refreshes
            role-aliased VIP records on its next tick.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {memberId === null ? (
            <>
              <div className="space-y-1.5">
                <Label className="text-xs">Role</Label>
                <Select value="replica" disabled>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="replica">Replica</SelectItem>
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  Only replicas can be added at runtime. The monitor is a
                  singleton; the primary is elected by pg_auto_failover.
                </p>
              </div>

              <div className="space-y-1.5">
                <Label className="text-xs">Node</Label>
                <Select value={nodeId} onValueChange={setNodeId}>
                  <SelectTrigger>
                    <SelectValue placeholder="Select node..." />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="control-plane">
                      <div className="flex items-center gap-2">
                        <Server className="h-3 w-3" />
                        Control Plane
                      </div>
                    </SelectItem>
                    {activeNodes.map((node) => (
                      <SelectItem key={node.id} value={String(node.id)}>
                        <div className="flex items-center gap-2">
                          <Server className="h-3 w-3" />
                          {node.name}
                          <span className="text-muted-foreground text-xs">
                            ({node.private_address})
                          </span>
                        </div>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  For true high availability, place the new replica on a
                  different node than the current primary.
                </p>
              </div>

              <div className="flex justify-end gap-2">
                <Button
                  variant="outline"
                  asChild
                  disabled={addMember.isPending}
                >
                  <Link to={`/storage/${id}`}>Cancel</Link>
                </Button>
                <Button
                  onClick={() => addMember.mutate()}
                  disabled={addMember.isPending}
                >
                  {addMember.isPending && (
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  )}
                  Start provisioning
                </Button>
              </div>
            </>
          ) : (
            <ProvisioningTimeline
              member={member}
              serviceId={Number(id)}
              onDone={() => navigate(`/storage/${id}`)}
              isFailed={isFailed}
              isDone={isDone}
            />
          )}
        </CardContent>
      </Card>
    </div>
  )
}

function ProvisioningTimeline({
  member,
  serviceId,
  onDone,
  isFailed,
  isDone,
}: {
  member: WireMember | undefined
  serviceId: number
  onDone: () => void
  isFailed: boolean
  isDone: boolean
}) {
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3 text-sm">
        <span className="font-mono">
          {member?.container_name ?? 'Reserving…'}
        </span>
        {member?.node_id && (
          <span className="text-muted-foreground">
            on node {member.node_id}
          </span>
        )}
        {member?.compute_ip && (
          <span className="text-muted-foreground">({member.compute_ip})</span>
        )}
      </div>

      <ol className="space-y-2">
        {PROVISIONING_STEPS.map((step) => {
          const status = stepStatus(member, step.id)
          return (
            <li
              key={step.id}
              className="flex gap-3 p-3 rounded-md border border-border"
            >
              <div className="mt-0.5 flex-shrink-0">
                {status === 'done' && (
                  <CheckCircle2 className="h-5 w-5 text-emerald-500" />
                )}
                {status === 'active' && (
                  <Loader2 className="h-5 w-5 animate-spin text-primary" />
                )}
                {status === 'pending' && (
                  <Circle className="h-5 w-5 text-muted-foreground" />
                )}
                {status === 'failed' && (
                  <AlertCircle className="h-5 w-5 text-destructive" />
                )}
              </div>
              <div className="flex flex-col">
                <span
                  className={
                    status === 'done'
                      ? 'text-foreground'
                      : status === 'active'
                        ? 'text-foreground font-medium'
                        : status === 'failed'
                          ? 'text-destructive'
                          : 'text-muted-foreground'
                  }
                >
                  {step.label}
                </span>
                <span className="text-xs text-muted-foreground">
                  {step.description}
                </span>
              </div>
            </li>
          )
        })}
      </ol>

      {isFailed && member?.provisioning_error && (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>
            <div className="font-medium">Provisioning failed</div>
            <div className="mt-1 text-sm font-mono whitespace-pre-wrap break-words">
              {member.provisioning_error}
            </div>
          </AlertDescription>
        </Alert>
      )}

      <div className="flex justify-end gap-2 pt-2">
        {isDone || isFailed ? (
          <Button onClick={onDone}>
            {isDone ? 'Done — back to service' : 'Back to service'}
          </Button>
        ) : (
          <Button variant="outline" asChild>
            <Link to={`/storage/${serviceId}`}>
              Hide this page (provisioning continues)
            </Link>
          </Button>
        )}
      </div>
    </div>
  )
}
