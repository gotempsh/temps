import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  updateEnvironmentSettingsMutation,
  adminListNodesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { BranchSelector } from '@/components/deployments/BranchSelector'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useMutation, useQuery } from '@tanstack/react-query'
import { GitBranch, KeyRound, Loader2, Moon, Network, Plus, Shield, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

interface EnvironmentConfigurationCardProps {
  project: ProjectResponse
  environment: EnvironmentResponse
  onUpdate: () => void
}

interface SecurityConfig {
  enabled?: boolean
  headers?: {
    preset?: string
    contentSecurityPolicy?: string
    xFrameOptions?: string
    strictTransportSecurity?: string
    referrerPolicy?: string
  }
  rateLimiting?: {
    maxRequestsPerMinute?: number
    maxRequestsPerHour?: number
    whitelistIps?: string[]
    blacklistIps?: string[]
  }
}

/** Select value for the tri-state environment attack-mode override. */
type AttackModeSelect = 'inherit' | 'on' | 'off'

/**
 * Map the environment's nullable `attack_mode` to the select value.
 * `null`/`undefined` → "inherit" (use the project setting); `true` → "on";
 * `false` → "off". Keeping these distinct is what lets an environment opt out
 * of (or into) attack mode independently of the project default.
 */
function attackModeToSelect(value: boolean | null | undefined): AttackModeSelect {
  if (value === true) return 'on'
  if (value === false) return 'off'
  return 'inherit'
}

/**
 * Map the select value back to the API payload. "inherit" sends `null` so the
 * column is cleared and the project setting applies; "on"/"off" send the
 * explicit override. Sending `false` for "off" is intentional — it forces
 * attack mode off even when the project has it on.
 */
function attackModeToPayload(value: AttackModeSelect): boolean | null {
  if (value === 'on') return true
  if (value === 'off') return false
  return null
}

export function EnvironmentConfigurationCard({
  project,
  environment,
  onUpdate,
}: EnvironmentConfigurationCardProps) {
  const nodesQuery = useQuery({
    ...adminListNodesOptions(),
  })
  const activeNodes: NodeInfoResponse[] =
    nodesQuery.data?.nodes?.filter((n) => n.status === 'active') ?? []

  // CPU is stored as microcores in the API/DB (1_000_000 = 1 core).
  // Convert to cores for display so users can type "1", "0.5", "2".
  const microToCores = (u?: number | null): string =>
    u != null ? (u / 1_000_000).toString() : ''

  const [formData, setFormData] = useState({
    branch: environment.branch ?? '',
    cpu_request: microToCores(environment.deployment_config?.cpuRequest),
    cpu_limit: microToCores(environment.deployment_config?.cpuLimit),
    memory_request:
      environment.deployment_config?.memoryRequest?.toString() ?? '',
    memory_limit: environment.deployment_config?.memoryLimit?.toString() ?? '',
    replicas: environment.deployment_config?.replicas?.toString() ?? '1',
    exposed_port: environment.deployment_config?.exposedPort?.toString() ?? '',
    // Tri-state attack mode: 'inherit' (null → use the project setting),
    // 'on' (true → force on) or 'off' (false → force off). Map the nullable
    // boolean from the API to the select value.
    attack_mode: attackModeToSelect(environment.attack_mode),
    protected: environment.protected ?? false,
    anti_affinity: environment.deployment_config?.antiAffinity ?? true,
    target_nodes: (environment.deployment_config?.targetNodes ?? []) as number[],
    target_labels: (environment.deployment_config?.targetLabels ?? {}) as Record<string, string>,
    automatic_deploy: environment.deployment_config?.automaticDeploy ?? true,
    on_demand: environment.deployment_config?.onDemand ?? false,
    idle_timeout_seconds: environment.deployment_config?.idleTimeoutSeconds?.toString() ?? '300',
    wake_timeout_seconds: environment.deployment_config?.wakeTimeoutSeconds?.toString() ?? '30',
    password_enabled: environment.deployment_config?.security?.passwordProtection?.enabled ?? false,
    password: '',
    security: {
      enabled: environment.deployment_config?.security?.enabled ?? false,
      headers: {
        preset:
          environment.deployment_config?.security?.headers?.preset ?? '',
        contentSecurityPolicy:
          environment.deployment_config?.security?.headers
            ?.contentSecurityPolicy ?? '',
        xFrameOptions:
          environment.deployment_config?.security?.headers?.xFrameOptions ?? '',
        strictTransportSecurity:
          environment.deployment_config?.security?.headers
            ?.strictTransportSecurity ?? '',
        referrerPolicy:
          environment.deployment_config?.security?.headers?.referrerPolicy ?? '',
      },
      rateLimiting: {
        maxRequestsPerMinute:
          environment.deployment_config?.security?.rateLimiting
            ?.maxRequestsPerMinute ?? undefined,
        maxRequestsPerHour:
          environment.deployment_config?.security?.rateLimiting
            ?.maxRequestsPerHour ?? undefined,
      },
    } as SecurityConfig,
  })

  // Label editing state
  const [newLabelKey, setNewLabelKey] = useState('')
  const [newLabelValue, setNewLabelValue] = useState('')

  // Sync form data when environment changes
  useEffect(() => {
    setFormData({
      branch: environment.branch ?? '',
      cpu_request: microToCores(environment.deployment_config?.cpuRequest),
      cpu_limit: microToCores(environment.deployment_config?.cpuLimit),
      memory_request:
        environment.deployment_config?.memoryRequest?.toString() ?? '',
      memory_limit:
        environment.deployment_config?.memoryLimit?.toString() ?? '',
      replicas: environment.deployment_config?.replicas?.toString() ?? '1',
      exposed_port: environment.deployment_config?.exposedPort?.toString() ?? '',
      attack_mode: attackModeToSelect(environment.attack_mode),
      protected: environment.protected ?? false,
      anti_affinity: environment.deployment_config?.antiAffinity ?? true,
      target_nodes: (environment.deployment_config?.targetNodes ?? []) as number[],
      target_labels: (environment.deployment_config?.targetLabels ?? {}) as Record<string, string>,
      automatic_deploy: environment.deployment_config?.automaticDeploy ?? true,
    on_demand: environment.deployment_config?.onDemand ?? false,
      idle_timeout_seconds: environment.deployment_config?.idleTimeoutSeconds?.toString() ?? '300',
      wake_timeout_seconds: environment.deployment_config?.wakeTimeoutSeconds?.toString() ?? '30',
      password_enabled: environment.deployment_config?.security?.passwordProtection?.enabled ?? false,
      password: '',
      security: {
        enabled: environment.deployment_config?.security?.enabled ?? false,
        headers: {
          preset:
            environment.deployment_config?.security?.headers?.preset ?? '',
          contentSecurityPolicy:
            environment.deployment_config?.security?.headers
              ?.contentSecurityPolicy ?? '',
          xFrameOptions:
            environment.deployment_config?.security?.headers?.xFrameOptions ??
            '',
          strictTransportSecurity:
            environment.deployment_config?.security?.headers
              ?.strictTransportSecurity ?? '',
          referrerPolicy:
            environment.deployment_config?.security?.headers?.referrerPolicy ??
            '',
        },
        rateLimiting: {
          maxRequestsPerMinute:
            environment.deployment_config?.security?.rateLimiting
              ?.maxRequestsPerMinute ?? undefined,
          maxRequestsPerHour:
            environment.deployment_config?.security?.rateLimiting
              ?.maxRequestsPerHour ?? undefined,
        },
      } as SecurityConfig,
    })
  }, [environment])

  const updateEnvironmentSettings = useMutation({
    ...updateEnvironmentSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update environment configuration',
    },
    onSuccess: () => {
      toast.success('Environment configuration updated successfully')
      onUpdate()
    },
  })

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()

    updateEnvironmentSettings.mutateAsync({
      path: {
        project_id: project.id,
        env_id: environment.id,
      },
      body: {
        branch: formData.branch.trim() !== '' ? formData.branch : null,
        cpu_request: formData.cpu_request
          ? Math.round(parseFloat(formData.cpu_request) * 1_000_000)
          : null,
        cpu_limit: formData.cpu_limit
          ? Math.round(parseFloat(formData.cpu_limit) * 1_000_000)
          : null,
        memory_request: formData.memory_request
          ? parseInt(formData.memory_request)
          : null,
        memory_limit: formData.memory_limit
          ? parseInt(formData.memory_limit)
          : null,
        replicas: formData.replicas ? parseInt(formData.replicas) : null,
        exposed_port: formData.exposed_port
          ? parseInt(formData.exposed_port)
          : null,
        protected: formData.protected,
        automatic_deploy: formData.automatic_deploy,
        // Tri-state: null clears the override (inherit project), true/false force it.
        attack_mode: attackModeToPayload(formData.attack_mode),
        anti_affinity: formData.anti_affinity,
        target_nodes:
          formData.target_nodes.length > 0 ? formData.target_nodes : null,
        target_labels:
          Object.keys(formData.target_labels).length > 0
            ? formData.target_labels
            : null,
        on_demand: formData.on_demand,
        idle_timeout_seconds: formData.idle_timeout_seconds
          ? parseInt(formData.idle_timeout_seconds)
          : null,
        wake_timeout_seconds: formData.wake_timeout_seconds
          ? parseInt(formData.wake_timeout_seconds)
          : null,
        security: formData.security,
        password: formData.password_enabled
          ? (formData.password || null)
          : (formData.password_enabled === false && environment.deployment_config?.security?.passwordProtection?.enabled
            ? ''
            : null),
      },
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <GitBranch className="h-5 w-5" />
          Configuration
        </CardTitle>
        <CardDescription>
          Configure Git branch, compute resources, and scaling for this
          environment
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit}>
          <div className="space-y-8">
            {/* Git Configuration Section */}
            <div className="border-b pb-6">
              <h3 className="text-sm font-medium mb-4">Git Configuration</h3>
              <div>
                <Label>Branch Name</Label>
                <div className="mt-2">
                  <BranchSelector
                    repoOwner={project.repo_owner || ''}
                    repoName={project.repo_name || ''}
                    connectionId={project.git_provider_connection_id || 0}
                    defaultBranch={project.main_branch}
                    value={formData.branch}
                    onChange={(branch) =>
                      setFormData((prev) => ({ ...prev, branch }))
                    }
                  />
                </div>
                <p className="text-xs text-muted-foreground mt-2">
                  Deployments will be triggered from this branch
                </p>
              </div>

              {/* Deploy on push toggle */}
              <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg mt-4">
                <div className="flex-1 min-w-0">
                  <Label className="text-sm font-medium">Deploy on push</Label>
                  <p className="text-xs text-muted-foreground">
                    Automatically deploy when a commit is pushed to this branch. Disable to deploy on demand only.
                  </p>
                </div>
                <Switch
                  checked={formData.automatic_deploy}
                  onCheckedChange={(checked) =>
                    setFormData((prev) => ({ ...prev, automatic_deploy: checked }))
                  }
                />
              </div>
            </div>

            {/* CPU Resources */}
            <div>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
                <div className="space-y-4">
                  <h3 className="text-sm font-medium">CPU Resources</h3>
                  <div className="space-y-4">
                    <div>
                      <Label>CPU Request (cores)</Label>
                      <Input
                        type="number"
                        step="any"
                        min="0.01"
                        value={formData.cpu_request}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            cpu_request: e.target.value,
                          }))
                        }
                        placeholder="e.g., 0.1"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Minimum CPU cores (e.g., 0.25, 0.5, 1, 2)
                      </p>
                    </div>
                    <div>
                      <div className="flex items-center justify-between">
                        <Label>CPU Limit (cores)</Label>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className="h-auto px-1 py-0 text-xs text-muted-foreground"
                          disabled={formData.cpu_limit === ''}
                          onClick={() =>
                            setFormData((prev) => ({ ...prev, cpu_limit: '' }))
                          }
                        >
                          No limit
                        </Button>
                      </div>
                      <Input
                        type="number"
                        step="any"
                        min="0.01"
                        value={formData.cpu_limit}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            cpu_limit: e.target.value,
                          }))
                        }
                        placeholder="No limit"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Maximum CPU cores (e.g., 0.5, 1, 2). Leave empty for no
                        limit.
                      </p>
                    </div>
                  </div>
                </div>

                {/* Memory Resources */}
                <div className="space-y-4">
                  <h3 className="text-sm font-medium">Memory Resources</h3>
                  <div className="space-y-4">
                    <div>
                      <Label>Memory Request (MB)</Label>
                      <Input
                        type="number"
                        value={formData.memory_request}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            memory_request: e.target.value,
                          }))
                        }
                        placeholder="e.g., 128"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Minimum memory allocation
                      </p>
                    </div>
                    <div>
                      <div className="flex items-center justify-between">
                        <Label>Memory Limit (MB)</Label>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className="h-auto px-1 py-0 text-xs text-muted-foreground"
                          disabled={formData.memory_limit === ''}
                          onClick={() =>
                            setFormData((prev) => ({
                              ...prev,
                              memory_limit: '',
                            }))
                          }
                        >
                          No limit
                        </Button>
                      </div>
                      <Input
                        type="number"
                        value={formData.memory_limit}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            memory_limit: e.target.value,
                          }))
                        }
                        placeholder="No limit"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Maximum memory allocation. Leave empty for no limit.
                      </p>
                    </div>
                  </div>
                </div>
              </div>
            </div>

            {/* Scaling & Network */}
            <div className="border-t pt-6">
              <h3 className="text-sm font-medium mb-4">Scaling & Network</h3>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
                <div>
                  <Label>Replicas</Label>
                  <Input
                    type="number"
                    min="1"
                    value={formData.replicas}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        replicas: e.target.value,
                      }))
                    }
                    placeholder="e.g., 1"
                  />
                  <p className="text-xs text-muted-foreground mt-1">
                    Number of container instances
                  </p>
                </div>

                <div>
                  <Label>Exposed Port (Override)</Label>
                  <Input
                    type="number"
                    min="1"
                    max="65535"
                    value={formData.exposed_port}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        exposed_port: e.target.value,
                      }))
                    }
                    placeholder="Auto-detected from image"
                  />
                  <p className="text-xs text-muted-foreground mt-1">
                    Override the port for this environment. Priority: Image
                    EXPOSE → This value → Project port → Default (3000)
                  </p>
                </div>
              </div>
            </div>

            {/* On-Demand (Scale-to-Zero) */}
            <div className="border-t pt-6">
              <div className="flex items-center gap-2 mb-4">
                <Moon className="h-4 w-4" />
                <h3 className="text-sm font-medium">On-Demand (Scale-to-Zero)</h3>
              </div>
              <div className="space-y-4">
                <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg">
                  <div className="flex-1 min-w-0">
                    <Label className="text-sm font-medium">Enable On-Demand</Label>
                    <p className="text-xs text-muted-foreground">
                      Automatically stop containers after idle timeout
                      and restart on the next request.
                    </p>
                  </div>
                  <Switch
                    checked={formData.on_demand}
                    onCheckedChange={(checked) =>
                      setFormData((prev) => ({
                        ...prev,
                        on_demand: checked,
                      }))
                    }
                  />
                </div>

                {formData.on_demand && (
                  <div className="space-y-4 p-4 border rounded-lg bg-muted/30">
                    <div>
                      <Label>Idle Timeout (seconds)</Label>
                      <Input
                        type="number"
                        min="60"
                        max="86400"
                        value={formData.idle_timeout_seconds}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            idle_timeout_seconds: e.target.value,
                          }))
                        }
                        placeholder="300"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Seconds of inactivity before containers are stopped (60–86400). Default: 300 (5 minutes).
                      </p>
                    </div>
                    <div>
                      <Label>Wake Timeout (seconds)</Label>
                      <Input
                        type="number"
                        min="5"
                        max="120"
                        value={formData.wake_timeout_seconds}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            wake_timeout_seconds: e.target.value,
                          }))
                        }
                        placeholder="30"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Maximum seconds to wait for containers to start when waking (5–120). Default: 30.
                      </p>
                    </div>
                    {environment.sleeping && (
                      <div className="flex items-center gap-2 p-2 rounded-md bg-yellow-500/10 border border-yellow-500/20 text-yellow-600 dark:text-yellow-400 text-xs">
                        <Moon className="h-3.5 w-3.5" />
                        This environment is currently sleeping. It will wake on the next request.
                      </div>
                    )}
                  </div>
                )}
              </div>
            </div>

            {/* Node Scheduling */}
            {activeNodes.length > 0 && (
              <div className="border-t pt-6">
                <div className="flex items-center gap-2 mb-4">
                  <Network className="h-4 w-4" />
                  <h3 className="text-sm font-medium">Node Scheduling</h3>
                </div>
                <div className="space-y-4">
                  {/* Protected environment toggle */}
                  <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg">
                    <div className="flex-1 min-w-0">
                      <Label className="text-sm font-medium flex items-center gap-1.5">
                        <Shield className="h-4 w-4 shrink-0" />
                        Protected
                      </Label>
                      <p className="text-xs text-muted-foreground">
                        Git pushes will not auto-deploy. Deployments must be promoted from another environment.
                      </p>
                    </div>
                    <Switch
                      checked={formData.protected}
                      onCheckedChange={(checked) =>
                        setFormData((prev) => ({
                          ...prev,
                          protected: checked,
                        }))
                      }
                    />
                  </div>

                  {/* Anti-affinity toggle */}
                  <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg">
                    <div className="flex-1 min-w-0">
                      <Label className="text-sm font-medium">
                        Anti-affinity
                      </Label>
                      <p className="text-xs text-muted-foreground">
                        Spread replicas across different nodes (best-effort).
                      </p>
                    </div>
                    <Switch
                      checked={formData.anti_affinity}
                      onCheckedChange={(checked) =>
                        setFormData((prev) => ({
                          ...prev,
                          anti_affinity: checked,
                        }))
                      }
                    />
                  </div>

                  {/* Target Nodes */}
                  <div>
                    <Label className="text-sm font-medium">Target Nodes</Label>
                    <p className="text-xs text-muted-foreground mb-2">
                      Restrict deployments to specific nodes. Leave empty to use
                      all active nodes.
                    </p>
                    <div className="space-y-2">
                      {activeNodes.map((node) => {
                        const isSelected = formData.target_nodes.includes(
                          node.id
                        )
                        return (
                          <label
                            key={node.id}
                            className="flex items-center gap-3 p-2 border rounded-lg cursor-pointer hover:bg-muted/50"
                          >
                            <Checkbox
                              checked={isSelected}
                              onCheckedChange={(checked) => {
                                setFormData((prev) => ({
                                  ...prev,
                                  target_nodes: checked
                                    ? [...prev.target_nodes, node.id]
                                    : prev.target_nodes.filter(
                                        (id) => id !== node.id
                                      ),
                                }))
                              }}
                            />
                            <div className="flex-1 min-w-0">
                              <span className="text-sm font-medium">
                                {node.name}
                              </span>
                              <span className="text-xs text-muted-foreground ml-2">
                                {node.private_address}
                              </span>
                            </div>
                            <Badge
                              variant="secondary"
                              className="text-[10px] shrink-0"
                            >
                              {node.role}
                            </Badge>
                          </label>
                        )
                      })}
                    </div>
                    {formData.target_nodes.length > 0 && (
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        className="mt-1 text-xs"
                        onClick={() =>
                          setFormData((prev) => ({
                            ...prev,
                            target_nodes: [],
                          }))
                        }
                      >
                        Clear selection
                      </Button>
                    )}
                  </div>

                  {/* Target Labels */}
                  <div>
                    <Label className="text-sm font-medium">
                      Label Selectors
                    </Label>
                    <p className="text-xs text-muted-foreground mb-2">
                      Only deploy to nodes matching these labels. All keys must
                      match (AND logic).
                    </p>

                    {/* Existing labels */}
                    {Object.entries(formData.target_labels).length > 0 && (
                      <div className="flex flex-wrap gap-2 mb-2">
                        {Object.entries(formData.target_labels).map(
                          ([key, value]) => (
                            <Badge
                              key={key}
                              variant="secondary"
                              className="gap-1 pr-1"
                            >
                              {key}={value}
                              <button
                                type="button"
                                className="ml-1 hover:text-destructive"
                                onClick={() => {
                                  setFormData((prev) => {
                                    const labels = { ...prev.target_labels }
                                    delete labels[key]
                                    return { ...prev, target_labels: labels }
                                  })
                                }}
                              >
                                <X className="h-3 w-3" />
                              </button>
                            </Badge>
                          )
                        )}
                      </div>
                    )}

                    {/* Add label */}
                    <div className="flex flex-col sm:flex-row gap-2">
                      <Input
                        placeholder="Key (e.g., region)"
                        value={newLabelKey}
                        onChange={(e) => setNewLabelKey(e.target.value)}
                        className="flex-1"
                      />
                      <Input
                        placeholder="Value (e.g., us)"
                        value={newLabelValue}
                        onChange={(e) => setNewLabelValue(e.target.value)}
                        className="flex-1"
                      />
                      <Button
                        type="button"
                        variant="outline"
                        size="icon"
                        className="shrink-0 self-end sm:self-auto"
                        disabled={!newLabelKey.trim() || !newLabelValue.trim()}
                        onClick={() => {
                          if (newLabelKey.trim() && newLabelValue.trim()) {
                            setFormData((prev) => ({
                              ...prev,
                              target_labels: {
                                ...prev.target_labels,
                                [newLabelKey.trim()]: newLabelValue.trim(),
                              },
                            }))
                            setNewLabelKey('')
                            setNewLabelValue('')
                          }
                        }}
                      >
                        <Plus className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>
                </div>
              </div>
            )}

            {/* Security Configuration */}
            <div className="border-t pt-6">
              <div className="flex items-center gap-2 mb-4">
                <Shield className="h-4 w-4" />
                <h3 className="text-sm font-medium">Security</h3>
              </div>

              <div className="space-y-4">
                <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg">
                  <div className="flex-1 min-w-0">
                    <Label className="text-sm font-medium">Attack Mode</Label>
                    <p className="text-xs text-muted-foreground">
                      Require a CAPTCHA challenge for visitors. Inherit uses the
                      project default (currently{' '}
                      {project.attack_mode ? 'on' : 'off'}).
                    </p>
                  </div>
                  <Select
                    value={formData.attack_mode}
                    onValueChange={(value) =>
                      setFormData((prev) => ({
                        ...prev,
                        attack_mode: value as AttackModeSelect,
                      }))
                    }
                  >
                    <SelectTrigger className="w-full sm:w-[180px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="inherit">
                        Inherit ({project.attack_mode ? 'on' : 'off'})
                      </SelectItem>
                      <SelectItem value="on">On</SelectItem>
                      <SelectItem value="off">Off</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg">
                  <div className="flex-1 min-w-0">
                    <Label className="text-sm font-medium">
                      Security Headers
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      Enable security headers
                    </p>
                  </div>
                  <Switch
                    checked={formData.security?.enabled ?? false}
                    onCheckedChange={(checked) =>
                      setFormData((prev) => ({
                        ...prev,
                        security: {
                          ...prev.security,
                          enabled: checked,
                        },
                      }))
                    }
                  />
                </div>

                {formData.security?.enabled && (
                  <div className="space-y-4 p-4 border rounded-lg bg-muted/30">
                    <div>
                      <Label>Header Preset</Label>
                      <Select
                        value={formData.security?.headers?.preset ?? ''}
                        onValueChange={(value) =>
                          setFormData((prev) => ({
                            ...prev,
                            security: {
                              ...prev.security,
                              headers: {
                                ...prev.security?.headers,
                                preset: value,
                              },
                            },
                          }))
                        }
                      >
                        <SelectTrigger>
                          <SelectValue placeholder="Select preset" />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="strict">Strict</SelectItem>
                          <SelectItem value="moderate">Moderate</SelectItem>
                          <SelectItem value="permissive">Permissive</SelectItem>
                        </SelectContent>
                      </Select>
                      <p className="text-xs text-muted-foreground mt-1">
                        Choose a preset or customize headers manually
                      </p>
                    </div>
                  </div>
                )}

                {/* Password Protection */}
                <div className="flex items-start sm:items-center gap-3 p-3 border rounded-lg">
                  <div className="flex-1 min-w-0">
                    <Label className="text-sm font-medium flex items-center gap-1.5">
                      <KeyRound className="h-4 w-4 shrink-0" />
                      Password Protection
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      Require a password to access this environment.
                    </p>
                  </div>
                  <Switch
                    checked={formData.password_enabled}
                    onCheckedChange={(checked) =>
                      setFormData((prev) => ({
                        ...prev,
                        password_enabled: checked,
                        password: checked ? prev.password : '',
                      }))
                    }
                  />
                </div>

                {formData.password_enabled && (
                  <div className="space-y-3 p-4 border rounded-lg bg-muted/30">
                    <div>
                      <Label>Password</Label>
                      <Input
                        type="password"
                        value={formData.password}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            password: e.target.value,
                          }))
                        }
                        placeholder={
                          environment.deployment_config?.security?.passwordProtection?.enabled
                            ? 'Leave empty to keep current password'
                            : 'Enter a password'
                        }
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        {environment.deployment_config?.security?.passwordProtection?.enabled
                          ? 'A password is currently set. Enter a new one to change it, or leave empty to keep the current password.'
                          : 'Set a password that visitors must enter to access this environment. The password is securely hashed.'}
                      </p>
                    </div>
                  </div>
                )}

                <div className="space-y-4 p-4 border rounded-lg">
                  <h4 className="text-sm font-medium">Rate Limiting</h4>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    <div>
                      <Label>Max Requests Per Minute</Label>
                      <Input
                        type="number"
                        value={
                          formData.security?.rateLimiting
                            ?.maxRequestsPerMinute ?? ''
                        }
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            security: {
                              ...prev.security,
                              rateLimiting: {
                                ...prev.security?.rateLimiting,
                                maxRequestsPerMinute: e.target.value
                                  ? parseInt(e.target.value)
                                  : undefined,
                              },
                            },
                          }))
                        }
                        placeholder="e.g., 600"
                      />
                    </div>
                    <div>
                      <Label>Max Requests Per Hour</Label>
                      <Input
                        type="number"
                        value={
                          formData.security?.rateLimiting
                            ?.maxRequestsPerHour ?? ''
                        }
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            security: {
                              ...prev.security,
                              rateLimiting: {
                                ...prev.security?.rateLimiting,
                                maxRequestsPerHour: e.target.value
                                  ? parseInt(e.target.value)
                                  : undefined,
                              },
                            },
                          }))
                        }
                        placeholder="e.g., 10000"
                      />
                    </div>
                  </div>
                </div>
              </div>
            </div>

            <Button
              type="submit"
              className="w-full sm:w-auto"
              disabled={updateEnvironmentSettings.isPending}
            >
              {updateEnvironmentSettings.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Save Configuration
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  )
}
