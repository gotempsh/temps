import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { updateEnvironmentSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
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
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

interface EnvironmentResourcesCardProps {
  project: ProjectResponse
  environment: EnvironmentResponse
  onUpdate: () => void
}

export function EnvironmentResourcesCard({
  project,
  environment,
  onUpdate,
}: EnvironmentResourcesCardProps) {
  const [formData, setFormData] = useState({
    cpu_request: environment.cpu_request?.toString() ?? '',
    cpu_limit: environment.cpu_limit?.toString() ?? '',
    memory_request: environment.memory_request?.toString() ?? '',
    memory_limit: environment.memory_limit?.toString() ?? '',
    replicas: environment.replicas?.toString() ?? '1',
    exposed_port: environment.exposed_port?.toString() ?? '',
  })
  const updateEnvironmentSettings = useMutation({
    ...updateEnvironmentSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update environment settings',
    },
    onSuccess: () => {
      toast.success('Environment settings have been updated successfully.')
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
      },
    })

    onUpdate()
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Resources & Scaling</CardTitle>
        <CardDescription>
          Configure compute resources, scaling, and network settings for this
          environment
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit}>
          <div className="space-y-6">
            {/* CPU Resources */}
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

            <Button
              type="submit"
              disabled={updateEnvironmentSettings.isPending}
            >
              {updateEnvironmentSettings.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Save Settings
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  )
}
