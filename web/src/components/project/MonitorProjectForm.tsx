// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type FormEvent } from 'react'
import { useNavigate } from 'react-router'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  createProjectMutation,
  getProjectsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'

export function MonitorProjectForm({ onCreated }: { onCreated?: () => void }) {
  const [name, setName] = useState('')
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const create = useMutation({
    ...createProjectMutation(),
    onSuccess: async (project) => {
      await queryClient.invalidateQueries({ queryKey: getProjectsQueryKey() })
      if (onCreated) onCreated()
      else navigate(`/projects/${project.slug}/integrations`)
    },
  })
  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!name.trim() || create.isPending) return
    create.mutate({ body: { name: name.trim(), source_type: 'external' } })
  }
  return (
    <form
      onSubmit={submit}
      className="space-y-6 rounded-lg border bg-card p-5"
      aria-label="Create monitoring project"
    >
      <div>
        <h2 className="text-lg font-semibold">
          Monitor an existing application
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">
          Collect analytics, errors, and traces from an application running
          anywhere. No repository or deployment required.
        </p>
      </div>
      <div className="space-y-2">
        <Label htmlFor="monitor-project-name">Project name</Label>
        <Input
          id="monitor-project-name"
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder="My application"
          required
          maxLength={100}
          autoComplete="off"
          disabled={create.isPending}
        />
      </div>
      <p className="text-sm text-muted-foreground">
        Next, choose an SDK or OpenTelemetry integration. You can add hosting to
        this project later and keep your collected data.
      </p>
      {create.isError && (
        <Alert variant="destructive">
          <AlertDescription>
            {create.error instanceof Error
              ? create.error.message
              : 'Could not create the project. Check your permissions and try again.'}
          </AlertDescription>
        </Alert>
      )}
      <Button type="submit" disabled={!name.trim() || create.isPending}>
        {create.isPending ? 'Creating project…' : 'Create monitoring project'}
      </Button>
    </form>
  )
}
