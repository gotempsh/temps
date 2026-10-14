// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import type { ProjectResponse, ProjectSecretResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  listProjectSecretsOptions,
  updateProjectSecretMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { useVariableChangeRedeploy } from '@/hooks/useVariableChangeRedeploy'
import { problemDetail } from '@/lib/api-problem'
import {
  composeServiceNamesFor,
  secretEditChangeScope,
  secretEditDefaults,
  secretEditHasChanges,
  secretEditSchema,
  secretEditToRequest,
  type SecretEditFormValues,
} from './secret-form'

function toggle<T>(items: readonly T[], item: T, checked: boolean): T[] {
  const without = items.filter((existing) => existing !== item)
  return checked ? [...without, item] : without
}

/**
 * Edit a project secret's value and scope. The stored value is never shown;
 * leaving the value blank keeps it, so scope can be changed without knowing
 * the secret. Saving replaces the secret's scope with what the form shows.
 */
export function SecretEditDialog({
  open,
  onOpenChange,
  project,
  secret,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  project: ProjectResponse
  /** The secret being edited; `null` while none is selected. */
  secret: ProjectSecretResponse | null
}) {
  const queryClient = useQueryClient()
  const secretKey = secret?.key ?? ''
  const notifyChange = useVariableChangeRedeploy(project)
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: open,
  })
  const environments = environmentsQuery.data ?? []

  const form = useForm<SecretEditFormValues>({
    resolver: zodResolver(secretEditSchema),
    defaultValues: secretEditDefaults(secret),
  })
  // Start every opening from what is stored now, never from a previous
  // abandoned edit.
  // Keyed on the secret's id, not its object: the list refetches in the
  // background, and a refetch must never wipe what the user is typing.
  const { reset } = form
  const secretRef = useRef(secret)
  useEffect(() => {
    secretRef.current = secret
  }, [secret])
  const secretId = secret?.id ?? null
  useEffect(() => {
    if (open) reset(secretEditDefaults(secretRef.current))
  }, [open, secretId, reset])

  const values = useWatch({
    control: form.control,
    defaultValue: secretEditDefaults(secret),
  }) as SecretEditFormValues
  const valueError = form.formState.errors.value?.message

  const updateMutation = useMutation({
    ...updateProjectSecretMutation(),
    meta: { errorTitle: `Failed to update secret ${secretKey}` },
    onSuccess: (_data, variables) => {
      if (!secret) return
      void queryClient.invalidateQueries({
        queryKey: listProjectSecretsOptions({
          path: { project_id: project.id },
          query: {},
        }).queryKey,
      })
      void queryClient.invalidateQueries({
        queryKey: ['secret-history', project.id, secret.id],
      })
      void notifyChange(
        `Secret ${secret.key} updated`,
        secretEditChangeScope(
          secret.environments.map((environment) => environment.id),
          variables.body.environment_ids ?? []
        )
      )
      onOpenChange(false)
    },
  })

  const handleSave = form.handleSubmit((submitted) => {
    if (updateMutation.isPending || !secret) return
    updateMutation.mutate({
      path: { project_id: project.id, secret_id: secret.id },
      body: secretEditToRequest(submitted),
    })
  })

  // Services the secret is already restricted to stay listed even when the
  // project's synced compose file no longer names them, so saving never
  // silently widens access.
  const composeServiceNames = Array.from(
    new Set([
      ...composeServiceNamesFor(project),
      ...(secret?.compose_services ?? []),
    ])
  ).sort()
  const isComposeProject = project.preset === 'docker-compose'
  const canSave =
    secretEditHasChanges(values, secret) && !updateMutation.isPending

  const setField = <K extends keyof SecretEditFormValues>(
    field: K,
    value: SecretEditFormValues[K]
  ) =>
    form.setValue(field as never, value as never, {
      shouldDirty: true,
      shouldValidate: true,
    })

  const handleOpenChange = (next: boolean) => {
    // A save in flight keeps the dialog: closing would hide its outcome.
    if (updateMutation.isPending) return
    // The dialog stays mounted, so a failed save's error would otherwise be
    // shown again the next time it opens.
    if (!next) updateMutation.reset()
    onOpenChange(next)
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            Edit secret <span className="font-mono">{secretKey}</span>
          </DialogTitle>
          <DialogDescription>
            The stored value is encrypted and cannot be displayed. Leave the
            value blank to keep it. Running containers keep the old value until
            the next deployment.
          </DialogDescription>
        </DialogHeader>
        <form
          id="edit-secret-form"
          onSubmit={(event) => {
            void handleSave(event)
          }}
          className="space-y-4"
        >
          <div className="space-y-1.5">
            <Label htmlFor="edit-secret-value">New value</Label>
            <Textarea
              id="edit-secret-value"
              value={values.value}
              onChange={(event) => setField('value', event.target.value)}
              placeholder="Leave blank to keep current value"
              className="font-mono text-xs"
              rows={4}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={valueError ? true : undefined}
              disabled={updateMutation.isPending}
            />
            {valueError ? (
              <p className="text-xs text-destructive">{valueError}</p>
            ) : (
              <p className="text-xs text-muted-foreground">
                Entering a value rotates the secret. Max 1 MiB.
              </p>
            )}
          </div>

          <div className="space-y-2">
            <Label>Environments</Label>
            {environmentsQuery.isPending ? (
              <div className="space-y-2">
                <Skeleton className="h-5 w-40" />
                <Skeleton className="h-5 w-32" />
              </div>
            ) : environmentsQuery.isError ? (
              <p role="alert" className="text-xs text-destructive">
                {problemDetail(
                  environmentsQuery.error,
                  'Could not load environments.'
                )}
              </p>
            ) : environments.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                No environments yet — the secret applies to all environments.
              </p>
            ) : (
              environments.map((environment) => (
                <label
                  key={environment.id}
                  className="flex cursor-pointer items-center gap-2 text-sm"
                >
                  <Checkbox
                    checked={values.environment_ids.includes(environment.id)}
                    disabled={updateMutation.isPending}
                    onCheckedChange={(checked) =>
                      setField(
                        'environment_ids',
                        toggle(
                          values.environment_ids,
                          environment.id,
                          checked === true
                        )
                      )
                    }
                  />
                  {environment.name}
                </label>
              ))
            )}
            <p className="text-xs text-muted-foreground">
              Leave empty to apply to all environments.
            </p>
          </div>

          {isComposeProject && composeServiceNames.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              This secret is mounted in every service. To restrict it to
              specific containers, sync this project&apos;s compose services
              from Git settings, then edit the secret.
            </p>
          ) : null}
          {composeServiceNames.length > 0 ? (
            <div className="space-y-2">
              <Label>Compose services</Label>
              {composeServiceNames.map((name) => (
                <label
                  key={name}
                  className="flex cursor-pointer items-center gap-2 text-sm"
                >
                  <Checkbox
                    checked={values.compose_services.includes(name)}
                    disabled={updateMutation.isPending}
                    onCheckedChange={(checked) =>
                      setField(
                        'compose_services',
                        toggle(values.compose_services, name, checked === true)
                      )
                    }
                  />
                  <span className="font-mono text-xs">{name}</span>
                </label>
              ))}
              <p className="text-xs text-muted-foreground">
                Leave empty to mount this secret in every service.
              </p>
            </div>
          ) : null}

          <label className="flex cursor-pointer items-center gap-2 text-sm">
            <Checkbox
              checked={values.include_in_preview}
              disabled={updateMutation.isPending}
              onCheckedChange={(checked) =>
                setField('include_in_preview', checked === true)
              }
            />
            Include in preview environments
          </label>

          {updateMutation.isError ? (
            <p role="alert" className="text-sm text-destructive">
              {problemDetail(
                updateMutation.error,
                `Could not update secret ${secretKey}. Please try again.`
              )}
            </p>
          ) : null}
        </form>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            disabled={updateMutation.isPending}
            onClick={() => handleOpenChange(false)}
          >
            Cancel
          </Button>
          <Button type="submit" form="edit-secret-form" disabled={!canSave}>
            {updateMutation.isPending ? 'Saving…' : 'Save changes'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
