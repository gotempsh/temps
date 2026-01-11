import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { updateEnvironmentSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { useMutation } from '@tanstack/react-query'
import { GitBranch, Loader2, Shield } from 'lucide-react'
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

export function EnvironmentConfigurationCard({
  project,
  environment,
  onUpdate,
}: EnvironmentConfigurationCardProps) {
  const [formData, setFormData] = useState({
    branch: environment.branch ?? '',
    cpu_request: environment.deployment_config?.cpuRequest?.toString() ?? '',
    cpu_limit: environment.deployment_config?.cpuLimit?.toString() ?? '',
    memory_request:
      environment.deployment_config?.memoryRequest?.toString() ?? '',
    memory_limit: environment.deployment_config?.memoryLimit?.toString() ?? '',
    replicas: environment.deployment_config?.replicas?.toString() ?? '1',
    exposed_port: environment.deployment_config?.exposedPort?.toString() ?? '',
    attack_mode: environment.attack_mode ?? false,
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

  // Sync form data when environment changes
  useEffect(() => {
    setFormData({
      branch: environment.branch ?? '',
      cpu_request: environment.deployment_config?.cpuRequest?.toString() ?? '',
      cpu_limit: environment.deployment_config?.cpuLimit?.toString() ?? '',
      memory_request:
        environment.deployment_config?.memoryRequest?.toString() ?? '',
      memory_limit:
        environment.deployment_config?.memoryLimit?.toString() ?? '',
      replicas: environment.deployment_config?.replicas?.toString() ?? '1',
      exposed_port: environment.deployment_config?.exposedPort?.toString() ?? '',
      attack_mode: environment.attack_mode ?? false,
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
          ? parseInt(formData.cpu_request)
          : null,
        cpu_limit: formData.cpu_limit ? parseInt(formData.cpu_limit) : null,
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
        attack_mode: formData.attack_mode,
        security: formData.security,
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
            </div>

            {/* CPU Resources */}
            <div>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
                <div className="space-y-4">
                  <h3 className="text-sm font-medium">CPU Resources</h3>
                  <div className="space-y-4">
                    <div>
                      <Label>CPU Request (millicores)</Label>
                      <Input
                        type="number"
                        value={formData.cpu_request}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            cpu_request: e.target.value,
                          }))
                        }
                        placeholder="e.g., 100"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Minimum CPU resources (1000m = 1 CPU core)
                      </p>
                    </div>
                    <div>
                      <Label>CPU Limit (millicores)</Label>
                      <Input
                        type="number"
                        value={formData.cpu_limit}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            cpu_limit: e.target.value,
                          }))
                        }
                        placeholder="e.g., 200"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Maximum CPU resources (1000m = 1 CPU core)
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
                      <Label>Memory Limit (MB)</Label>
                      <Input
                        type="number"
                        value={formData.memory_limit}
                        onChange={(e) =>
                          setFormData((prev) => ({
                            ...prev,
                            memory_limit: e.target.value,
                          }))
                        }
                        placeholder="e.g., 256"
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Maximum memory allocation
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

            {/* Security Configuration */}
            <div className="border-t pt-6">
              <div className="flex items-center gap-2 mb-4">
                <Shield className="h-4 w-4" />
                <h3 className="text-sm font-medium">Security</h3>
              </div>

              <div className="space-y-4">
                <div className="flex items-center gap-3 p-3 border rounded-lg">
                  <div className="flex-1">
                    <Label className="text-sm font-medium">Attack Mode</Label>
                    <p className="text-xs text-muted-foreground">
                      Enable attack mode for development/testing
                    </p>
                  </div>
                  <Switch
                    checked={formData.attack_mode}
                    onCheckedChange={(checked) =>
                      setFormData((prev) => ({
                        ...prev,
                        attack_mode: checked,
                      }))
                    }
                  />
                </div>

                <div className="flex items-center gap-3 p-3 border rounded-lg">
                  <div className="flex-1">
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
