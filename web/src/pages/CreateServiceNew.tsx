import {
  adminListNodesOptions,
  createServiceMutation,
  getProviderMetadataOptions,
  getProvidersMetadataOptions,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  ClusterMemberRequest,
  NodeInfoResponse,
  ServiceTypeRoute,
} from '@/api/client/types.gen'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Button } from '@/components/ui/button'
import { JsonSchemaForm } from '@/components/forms/JsonSchemaForm'
import { useServiceTypePreset } from '@/components/forms/ServiceTypePresets'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useMutation, useQuery } from '@tanstack/react-query'
import { customAlphabet } from 'nanoid'
import { ArrowLeft, CheckCircle2, Plus, Server, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'

// Create a custom nanoid with lowercase alphanumeric characters
const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

/** Service types that support HA cluster topology */
const CLUSTER_SERVICE_TYPES: ServiceTypeRoute[] = ['postgres']

/** Cluster roles the operator chooses at provisioning time.
 *
 * "Primary" used to be in this list, but pg_auto_failover *elects* the
 * primary at runtime — the operator doesn't pick one. Storing one row
 * as `primary` produced lag bugs after every failover (UI showed two
 * primaries until the reconciler caught up). The new model: every data
 * node is a `replica` at config time, and the live primary comes from
 * the monitor's FSM state (`live_state` on `ServiceMemberInfo`).
 */
const DEFAULT_CLUSTER_ROLES: Record<string, string[]> = {
  postgres: ['monitor', 'replica'],
}

const ROLE_DESCRIPTIONS: Record<string, string> = {
  monitor: 'pg_auto_failover monitor — coordinates failover',
  replica: 'Data node — pg_auto_failover elects one as primary at runtime',
}

function ClusterMemberConfig({
  members,
  onMembersChange,
  nodes,
  serviceType,
}: {
  members: ClusterMemberRequest[]
  onMembersChange: (members: ClusterMemberRequest[]) => void
  nodes: NodeInfoResponse[]
  serviceType: string
}) {
  const roles = DEFAULT_CLUSTER_ROLES[serviceType] || []

  const addMember = () => {
    // First member is the singleton monitor; everything after is a
    // replica (one of which pg_auto_failover will elect as primary).
    const hasMonitor = members.some((m) => m.role === 'monitor')
    const defaultRole = hasMonitor ? 'replica' : 'monitor'
    onMembersChange([...members, { role: defaultRole, node_id: null }])
  }

  const removeMember = (index: number) => {
    onMembersChange(members.filter((_, i) => i !== index))
  }

  const updateMember = (
    index: number,
    field: keyof ClusterMemberRequest,
    value: string | number | null
  ) => {
    const updated = [...members]
    if (field === 'node_id') {
      updated[index] = {
        ...updated[index],
        node_id: value === null ? null : Number(value),
      }
    } else {
      updated[index] = { ...updated[index], [field]: value as string }
    }
    onMembersChange(updated)
  }

  // Validation: a viable HA cluster needs the monitor + at least 2
  // replicas. With only one replica there's nothing for failover to
  // promote to.
  const hasMonitor = members.some((m) => m.role === 'monitor')
  const replicaCount = members.filter((m) => m.role === 'replica').length
  const hasEnoughReplicas = replicaCount >= 2
  const allHaveNodes = members.every((m) => m.node_id !== null)

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <Label>Cluster Members</Label>
          <p className="text-sm text-muted-foreground">
            Assign each member to a different node for true HA
          </p>
        </div>
        <Button type="button" variant="outline" size="sm" onClick={addMember}>
          <Plus className="h-4 w-4 mr-1" />
          Add Member
        </Button>
      </div>

      {members.length === 0 && (
        <div className="text-sm text-muted-foreground text-center py-4 border border-dashed rounded-md">
          No members configured. Add a monitor and at least two replicas
          (pg_auto_failover elects one as primary at runtime).
        </div>
      )}

      <div className="space-y-3">
        {members.map((member, index) => (
          <div
            key={index}
            className="flex items-start gap-3 p-3 border rounded-lg bg-muted/30"
          >
            <div className="flex items-center justify-center h-8 w-8 rounded-full bg-primary/10 text-primary text-xs font-medium flex-shrink-0 mt-1">
              {index + 1}
            </div>

            <div className="flex-1 grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label className="text-xs">Role</Label>
                <Select
                  value={member.role}
                  onValueChange={(v) => updateMember(index, 'role', v)}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {roles.map((role) => (
                      <SelectItem key={role} value={role}>
                        <span className="capitalize">{role}</span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {ROLE_DESCRIPTIONS[member.role] && (
                  <p className="text-xs text-muted-foreground">
                    {ROLE_DESCRIPTIONS[member.role]}
                  </p>
                )}
              </div>

              <div className="space-y-1.5">
                <Label className="text-xs">Node</Label>
                <Select
                  value={
                    member.node_id !== null && member.node_id !== undefined
                      ? String(member.node_id)
                      : 'control-plane'
                  }
                  onValueChange={(v) =>
                    updateMember(
                      index,
                      'node_id',
                      v === 'control-plane' ? null : Number(v)
                    )
                  }
                >
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
                    {nodes.map((node) => (
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
              </div>
            </div>

            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="h-8 w-8 flex-shrink-0 mt-1 text-muted-foreground hover:text-destructive"
              onClick={() => removeMember(index)}
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          </div>
        ))}
      </div>

      {members.length > 0 && (!hasMonitor || !hasEnoughReplicas) && (
        <div className="rounded-lg border border-amber-500/20 bg-amber-500/10 p-3 text-sm text-amber-800 dark:text-amber-200">
          A PostgreSQL cluster requires at least:{' '}
          <span className={hasMonitor ? 'line-through opacity-50' : 'font-medium'}>
            1 monitor
          </span>
          ,{' '}
          <span className={hasEnoughReplicas ? 'line-through opacity-50' : 'font-medium'}>
            2 replicas
          </span>
          {' '}(pg_auto_failover elects one as primary at runtime).
        </div>
      )}

      {hasMonitor && hasEnoughReplicas && !allHaveNodes && (
        <div className="rounded-lg border border-amber-500/20 bg-amber-500/10 p-3 text-sm text-amber-800 dark:text-amber-200">
          For true high availability, assign each member to a different node.
          Members on the control plane share the same machine.
        </div>
      )}

      {hasMonitor && hasEnoughReplicas && allHaveNodes && (
        <div className="rounded-lg border border-emerald-500/20 bg-emerald-500/10 p-3 text-sm text-emerald-800 dark:text-emerald-200">
          Cluster configuration looks good. Members will communicate via their
          private addresses.
        </div>
      )}
    </div>
  )
}

export function CreateService() {
  usePageTitle('Create Service')
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const serviceType = searchParams.get('type') as ServiceTypeRoute | null
  const { setBreadcrumbs } = useBreadcrumbs()

  const defaultName = useMemo(
    () => (serviceType ? `${serviceType}-${generateId()}` : ''),
    [serviceType]
  )

  const [serviceName, setServiceName] = useState(defaultName)
  const supportsCluster = useMemo(
    () =>
      serviceType !== null &&
      CLUSTER_SERVICE_TYPES.includes(serviceType as ServiceTypeRoute),
    [serviceType]
  )
  const [topology, setTopology] = useState<'standalone' | 'cluster'>(
    'standalone'
  )
  const [clusterMembers, setClusterMembers] = useState<ClusterMemberRequest[]>(
    []
  )

  const preset = useServiceTypePreset(serviceType)

  // Fetch available nodes to determine if cluster topology can be offered
  const { data: nodesResponse } = useQuery({
    ...adminListNodesOptions(),
    enabled: supportsCluster,
  })
  const nodes = useMemo(
    () =>
      (nodesResponse?.nodes ?? []).filter(
        (n: NodeInfoResponse) => n.status === 'active'
      ),
    [nodesResponse]
  )
  const hasWorkerNodes = useMemo(() => nodes.length > 0, [nodes])

  // Reset to standalone if no worker nodes are available
  useEffect(() => {
    if (!hasWorkerNodes && topology === 'cluster') {
      setTopology('standalone')
    }
  }, [hasWorkerNodes, topology])

  // When switching to cluster topology, pre-populate default members:
  //   - 1 monitor on the control plane (node_id = null)
  //   - 1 replica per active worker node, pinned to that node
  //
  // Operators can still tweak afterward, but the default lays out a
  // true-HA cluster with one data member per machine. We require at
  // least 2 replicas for a viable failover quorum; with 0 or 1 worker
  // nodes we fall back to the previous single-replica scaffold and
  // surface the warning blocks below.
  useEffect(() => {
    if (topology === 'cluster' && clusterMembers.length === 0 && serviceType) {
      if (DEFAULT_CLUSTER_ROLES[serviceType]) {
        const seeded: ClusterMemberRequest[] = [
          { role: 'monitor', node_id: null },
        ]
        if (nodes.length > 0) {
          for (const n of nodes) {
            seeded.push({ role: 'replica', node_id: n.id })
          }
        } else {
          // No worker nodes yet — leave a single empty replica row so
          // the operator sees what the cluster would look like, with
          // the warning block prompting them to add more.
          seeded.push({ role: 'replica', node_id: null })
        }
        setClusterMembers(seeded)
      }
    }
    if (topology === 'standalone') {
      setClusterMembers([])
    }
  }, [topology, serviceType, nodes])

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      { label: 'Create Service', href: '/storage/create' },
    ])
  }, [setBreadcrumbs])

  // Fetch provider metadata for display
  const { data: providerMetadata } = useQuery({
    ...getProviderMetadataOptions({
      path: {
        service_type: serviceType || '',
      },
    }),
    enabled: !!serviceType,
  })

  // Fetch JSON Schema for the selected service type
  const { data: jsonSchema, isLoading: isLoadingSchema } = useQuery({
    ...getServiceTypeParametersOptions({
      path: {
        service_type: serviceType || '',
      },
    }),
    enabled: !!serviceType,
  })

  const createServiceMut = useMutation({
    ...createServiceMutation(),
    meta: {
      errorTitle: 'Failed to create service',
    },
    onSuccess: (data) => {
      if (data.status === 'creating') {
        toast.success('Cluster creation started — tracking progress...')
      } else {
        toast.success('Service created successfully')
      }
      navigate(`/storage/${data.id}`)
    },
  })

  const handleSubmit = async (
    parameterValues: Record<string, string | null | number>
  ) => {
    if (!serviceName.trim()) {
      toast.error('Service name is required')
      return
    }

    // Keep numbers as numbers, convert null to empty strings
    const cleanedParameters: Record<string, any> = {}
    Object.entries(parameterValues).forEach(([key, value]) => {
      if (value === null) {
        cleanedParameters[key] = ''
      } else {
        // Keep the original type (string or number)
        cleanedParameters[key] = value
      }
    })

    // For cluster topology, remove standalone-only params so the backend uses HA defaults
    if (topology === 'cluster') {
      delete cleanedParameters['docker_image']
      delete cleanedParameters['host']
      delete cleanedParameters['port']
    }

    await createServiceMut.mutateAsync({
      body: {
        service_type: serviceType as ServiceTypeRoute,
        name: serviceName,
        parameters: cleanedParameters,
        ...(topology === 'cluster' && {
          topology: 'cluster',
          members: clusterMembers,
        }),
      },
    })
  }

  const { data: providers, isLoading: isLoadingProviders } = useQuery({
    ...getProvidersMetadataOptions(),
    enabled: !serviceType,
  })

  if (!serviceType) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
          <div className="space-y-1">
            <Link to="/storage">
              <Button variant="ghost" size="sm" className="gap-2 -ml-2 mb-2">
                <ArrowLeft className="h-4 w-4" />
                Back to Databases
              </Button>
            </Link>
            <h1 className="text-2xl font-semibold">Create Service</h1>
            <p className="text-muted-foreground">Choose a service type to get started.</p>
          </div>
          {isLoadingProviders ? (
            <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 gap-4">
              {[...Array(6)].map((_, i) => (
                <div key={i} className="h-24 bg-muted animate-pulse rounded-lg" />
              ))}
            </div>
          ) : (
            <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 gap-4">
              {providers?.map((provider) => (
                <button
                  key={provider.service_type}
                  type="button"
                  onClick={() => navigate(`/storage/create?type=${provider.service_type}`)}
                  className="flex items-center gap-4 rounded-lg border p-4 text-left hover:bg-accent transition-colors"
                >
                  <div
                    className="flex items-center justify-center rounded-md p-2 shrink-0"
                    style={{ backgroundColor: provider.color }}
                  >
                    <img
                      src={provider.icon_url}
                      alt={provider.display_name}
                      width={28}
                      height={28}
                      className="brightness-0 invert"
                    />
                  </div>
                  <div className="min-w-0">
                    <p className="font-medium">{provider.display_name}</p>
                    <p className="text-xs text-muted-foreground truncate">{provider.description}</p>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    )
  }

  if (isLoadingSchema) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
          <div className="space-y-4">
            <div className="h-8 w-1/3 bg-muted animate-pulse rounded" />
            <div className="space-y-3">
              {[...Array(5)].map((_, i) => (
                <div key={i} className="space-y-2">
                  <div className="h-4 w-1/4 bg-muted animate-pulse rounded" />
                  <div className="h-10 bg-muted animate-pulse rounded" />
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    )
  }

  if (!jsonSchema) {
    return null
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
        {/* Header with provider info */}
        <div className="space-y-4">
          <Link to="/storage">
            <Button variant="ghost" size="sm" className="gap-2">
              <ArrowLeft className="h-4 w-4" />
              Back to Storage
            </Button>
          </Link>

          {providerMetadata && (
            <div className="flex items-center gap-4">
              <div
                className="flex items-center justify-center rounded-md p-3"
                style={{ backgroundColor: providerMetadata.color }}
              >
                <img
                  src={providerMetadata.icon_url}
                  alt={`${providerMetadata.display_name} logo`}
                  width={40}
                  height={40}
                  className="rounded-md brightness-0 invert"
                />
              </div>
              <div>
                <h1 className="text-2xl font-semibold">
                  Create {providerMetadata.display_name} Service
                </h1>
                <p className="text-muted-foreground">
                  {providerMetadata.description}
                </p>
              </div>
            </div>
          )}
        </div>

        {/* Service Name Field */}
        <div className="space-y-2">
          <Label htmlFor="serviceName">
            Service Name
            <span className="text-destructive ml-1">*</span>
          </Label>
          <Input
            id="serviceName"
            value={serviceName}
            onChange={(e) => setServiceName(e.target.value)}
            placeholder={`my-${serviceType}`}
          />
          <p className="text-sm text-muted-foreground">
            A unique name to identify this service
          </p>
        </div>

        {/* Topology Selector (only for service types that support clustering AND when worker nodes exist) */}
        {supportsCluster && hasWorkerNodes && (
          <div className="space-y-4">
            <div className="space-y-2">
              <Label>Topology</Label>
              <p className="text-sm text-muted-foreground">
                Choose standalone for a single instance, or cluster for
                high-availability with automatic failover
              </p>
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <button
                type="button"
                onClick={() => setTopology('standalone')}
                className={`flex flex-col gap-1.5 rounded-lg border-2 p-4 text-left transition-colors ${
                  topology === 'standalone'
                    ? 'border-primary bg-primary/5'
                    : 'border-border hover:border-muted-foreground/50'
                }`}
              >
                <span className="font-medium text-sm">Standalone</span>
                <span className="text-xs text-muted-foreground">
                  Single container. Simple, fast. No failover.
                </span>
              </button>
              <button
                type="button"
                onClick={() => setTopology('cluster')}
                className={`flex flex-col gap-1.5 rounded-lg border-2 p-4 text-left transition-colors ${
                  topology === 'cluster'
                    ? 'border-primary bg-primary/5'
                    : 'border-border hover:border-muted-foreground/50'
                }`}
              >
                <span className="font-medium text-sm">
                  Cluster (HA)
                </span>
                <span className="text-xs text-muted-foreground">
                  Multi-node with pg_auto_failover. Requires 3+ nodes.
                </span>
              </button>
            </div>

            {topology === 'cluster' && (
              <>
                <p className="text-sm text-muted-foreground">
                  Docker image will be set to{' '}
                  <code className="font-mono text-xs bg-muted px-1 py-0.5 rounded">
                    gotempsh/postgres-ha:18-bookworm-walg
                  </code>{' '}
                  automatically (includes pg_auto_failover and WAL-G for backups).
                </p>
                <ClusterMemberConfig
                  members={clusterMembers}
                  onMembersChange={setClusterMembers}
                  nodes={nodes}
                  serviceType={serviceType}
                />
              </>
            )}
          </div>
        )}

        {/* Preview summary */}
        <ServicePreviewCard
          serviceName={serviceName}
          serviceType={serviceType}
          topology={supportsCluster ? topology : 'standalone'}
          schema={jsonSchema as any}
          dockerImageOverride={preset.overrides.docker_image}
          presetOwnsImage={preset.ownedFields.includes('docker_image')}
        />

        {/* Type-specific preset (version/engine pills) */}
        {topology === 'standalone' && preset.ui}

        {/* JSON Schema Form for Parameters */}
        <JsonSchemaForm
          schema={jsonSchema as any}
          onSubmit={handleSubmit}
          onCancel={() => navigate('/storage')}
          submitText={
            serviceName.trim()
              ? `Create ${serviceName.trim()}`
              : 'Create Service'
          }
          isSubmitting={createServiceMut.isPending}
          serviceType={serviceType}
          managedByTemps
          fieldOverrides={
            topology === 'standalone' ? preset.overrides : undefined
          }
          presetOwnedFields={
            topology === 'standalone' ? preset.ownedFields : undefined
          }
          hiddenFields={
            topology === 'cluster'
              ? ['host', 'port', 'docker_image']
              : []
          }
        />
      </div>
    </div>
  )
}

interface ServicePreviewCardProps {
  serviceName: string
  serviceType: ServiceTypeRoute
  topology: 'standalone' | 'cluster'
  schema: {
    properties: Record<string, { default?: string | null }>
  } | null
  dockerImageOverride?: string
  presetOwnsImage?: boolean
}

function ServicePreviewCard({
  serviceName,
  serviceType,
  topology,
  schema,
  dockerImageOverride,
  presetOwnsImage,
}: ServicePreviewCardProps) {
  const displayName = serviceName.trim() || `<unnamed>`
  let dockerImage: string | null
  if (topology === 'cluster' && serviceType === 'postgres') {
    // Must match DEFAULT_CLUSTER_IMAGE in
    // crates/temps-providers/src/externalsvc/postgres_cluster.rs.
    // Backend ignores this string — it's preview-only — but keep it
    // in sync so operators see what'll actually run.
    dockerImage = 'gotempsh/postgres-ha:18-bookworm-walg'
  } else if (presetOwnsImage) {
    // Preset owns the field — show what it resolved to, or a placeholder.
    dockerImage = dockerImageOverride || 'Custom (not set)'
  } else {
    dockerImage = schema?.properties?.docker_image?.default ?? null
  }

  const highlights: { label: string; value: string }[] = []
  if (topology === 'cluster') {
    highlights.push({ label: 'Topology', value: 'Cluster (HA)' })
  }
  if (dockerImage) {
    highlights.push({ label: 'Image', value: dockerImage })
  }
  highlights.push({ label: 'Port', value: 'Auto-assigned' })
  highlights.push({ label: 'Password', value: 'Auto-generated if empty' })

  return (
    <div className="rounded-lg border bg-muted/30 p-4">
      <div className="flex items-start gap-3">
        <CheckCircle2 className="h-5 w-5 text-emerald-500 flex-shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0 space-y-2">
          <div>
            <p className="text-sm">
              Will create{' '}
              <span className="font-mono font-medium">{displayName}</span>
            </p>
            <p className="text-xs text-muted-foreground">
              Defaults are filled in — click Create to provision.
            </p>
          </div>
          <dl className="grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-1 text-xs">
            {highlights.map((h) => (
              <div key={h.label} className="flex gap-1.5">
                <dt className="text-muted-foreground">{h.label}:</dt>
                <dd className="font-mono truncate" title={h.value}>
                  {h.value}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      </div>
    </div>
  )
}
