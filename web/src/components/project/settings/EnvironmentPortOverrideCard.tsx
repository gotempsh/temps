// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  updateEnvironmentSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

/**
 * Per-environment exposed-port override. Surfaced here (Build & Deploy →
 * Deploy) alongside the project-level default port in DeployDefaultsCard,
 * rather than only under each environment's own settings page, since both
 * describe the same deploy-time concern: which port the app listens on.
 * Priority: Image EXPOSE → this override → project default → 3000.
 */
export function EnvironmentPortOverrideCard({
  project,
}: {
  project: ProjectResponse
}) {
  const queryClient = useQueryClient()
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
  })
  const environments = environmentsQuery.data ?? []

  const [selectedEnvId, setSelectedEnvId] = useState<number | null>(null)
  const [portInput, setPortInput] = useState('')

  // Default to the first environment once the list loads.
  useEffect(() => {
    if (selectedEnvId != null) return
    // Picking a default selection once the async environments list resolves,
    // not reacting to a render.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (environments.length > 0) setSelectedEnvId(environments[0].id)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [environments])

  const selectedEnvironment = environments.find(
    (env: EnvironmentResponse) => env.id === selectedEnvId
  )

  // Reset the input whenever the selected environment (or its data) changes.
  useEffect(() => {
    // Syncing local input state to match the newly selected environment's
    // saved override, mirrors the reset-on-change pattern used elsewhere
    // (e.g. RedeploymentModal).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPortInput(
      selectedEnvironment?.deployment_config?.exposedPort?.toString() ?? ''
    )
  }, [selectedEnvironment])

  const updateSettings = useMutation({
    ...updateEnvironmentSettingsMutation(),
    meta: { errorTitle: 'Failed to update port override' },
  })

  const handleSave = async () => {
    if (!selectedEnvId) return
    await toast.promise(
      updateSettings.mutateAsync({
        path: { project_id: project.id, env_id: selectedEnvId },
        body: {
          exposed_port:
            portInput.trim() !== '' ? parseInt(portInput, 10) : null,
        },
      }),
      {
        loading: 'Updating port override...',
        success: 'Port override updated successfully',
        error: 'Failed to update port override',
      }
    )
    queryClient.invalidateQueries({ queryKey: ['environments'] })
    queryClient.invalidateQueries({ queryKey: ['environment'] })
  }

  return (
    <Card className="bg-background text-foreground">
      <CardHeader>
        <CardTitle>Environment Port Overrides</CardTitle>
        <CardDescription>
          Override the exposed port for a specific environment. Leave empty to
          inherit the default port above.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label>Environment</Label>
            <Select
              value={selectedEnvId?.toString() ?? ''}
              onValueChange={(value) => setSelectedEnvId(parseInt(value, 10))}
              disabled={environmentsQuery.isLoading}
            >
              <SelectTrigger>
                <SelectValue
                  placeholder={
                    environmentsQuery.isLoading
                      ? 'Loading...'
                      : 'Select environment...'
                  }
                />
              </SelectTrigger>
              <SelectContent>
                {environments.map((env: EnvironmentResponse) => (
                  <SelectItem key={env.id} value={env.id.toString()}>
                    {env.name || env.slug}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-2">
            <Label htmlFor="env-port-override">Exposed Port (Override)</Label>
            <Input
              id="env-port-override"
              type="number"
              min="1"
              max="65535"
              placeholder="Auto-detected from image"
              value={portInput}
              onChange={(e) => setPortInput(e.target.value)}
              disabled={!selectedEnvId}
            />
          </div>
        </div>
      </CardContent>
      <CardFooter>
        <Button
          onClick={handleSave}
          disabled={updateSettings.isPending || !selectedEnvId}
        >
          Save Override
        </Button>
      </CardFooter>
    </Card>
  )
}
