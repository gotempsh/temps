// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Callout, Field, Settings, CredentialProviderMark } from '@temps-sdk/ds'
import { Input, Textarea, Checkbox, Button } from '@temps-sdk/ui'
import { toast } from 'sonner'
import {
  createAccessService,
  updateAccessService,
  type AccessServiceResponse,
} from '@/api/client'
import { VaultRepositoryPicker } from './VaultRepositoryPicker'
import { serviceProviders } from './service-provider-registry'
import { problemDetail } from '@/lib/api-problem'
import {
  serviceEditorSchema,
  serviceDefaults,
  serviceBody,
  type AccessServiceValues,
} from './service-access-form'

export function AccessServiceEditor({
  projectId,
  vaultId,
  service,
  canUseProjectGithub = false,
  onClose,
  onSaved,
}: {
  vaultId?: string
  projectId?: number | null
  service?: AccessServiceResponse
  canUseProjectGithub?: boolean
  onClose: () => void
  onSaved: () => void
}) {
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])
  const [sourceMode, setSourceMode] = useState<'bearer' | 'api' | 'git'>(
    service?.policy.github_repository?.protocol ?? 'bearer'
  )
  const [connectionId, setConnectionId] = useState<number | null>(
    service?.policy.github_repository?.connection_id ?? null
  )
  const form = useForm<AccessServiceValues>({
    resolver: zodResolver(serviceEditorSchema(service, sourceMode)),
    defaultValues: serviceDefaults(service),
  })
  const provider = useWatch({ control: form.control, name: 'provider' })
  const github = useWatch({ control: form.control, name: 'githubRepository' })
  function selectRepository(
    reference: NonNullable<AccessServiceValues['githubRepository']>
  ) {
    const repo = `${reference.owner}/${reference.repository}`
    form.setValue('githubRepository', reference, {
      shouldDirty: true,
      shouldValidate: true,
    })
    form.setValue('provider', 'github', { shouldDirty: true })
    form.setValue(
      'origin',
      reference.protocol === 'git'
        ? 'https://github.com'
        : 'https://api.github.com',
      { shouldDirty: true }
    )
    form.setValue(
      'paths',
      reference.protocol === 'git'
        ? `/${repo}\n/${repo}.git`
        : `/repos/${repo}`,
      { shouldDirty: true }
    )
    form.setValue(
      'methods',
      reference.protocol === 'git' ? ['GET', 'POST'] : ['GET', 'HEAD'],
      { shouldDirty: true }
    )
    form.setValue('credential', '')
    form.setValue('clearCredential', false)
    if (!form.getValues('name'))
      form.setValue(
        'name',
        reference.protocol === 'git'
          ? 'GitHub clone/fetch'
          : 'GitHub repository API',
        { shouldDirty: true }
      )
  }
  const save = useMutation({
    mutationFn: async (values: AccessServiceValues) => {
      if (!projectId)
        throw new Error('Select a project before saving a service')
      const body = {
        ...serviceBody(values, service?.version),
        vault_id: service ? undefined : vaultId,
      }
      if (service)
        return updateAccessService({
          path: { project_id: projectId, service_id: service.id },
          body,
          throwOnError: true,
        })
      return createAccessService({
        path: { project_id: projectId },
        body,
        throwOnError: true,
      })
    },
    onSuccess: () => {
      // Refresh shared data even if the user already left this page.
      onSaved()
      if (!mounted.current) return
      form.reset(serviceDefaults())
      toast.success('Service saved', {
        description:
          'Review and approve a time-limited grant from the sandbox to connect.',
      })
      onClose()
    },
  })
  return (
    <Settings
      title={service ? 'Edit service' : 'Add service'}
      description="Save a scoped service. Connecting a sandbox requires a separate approval."
      onCancel={() => {
        if (!save.isPending) onClose()
      }}
      onSubmit={form.handleSubmit((values) => {
        if (!save.isPending && (!github || canUseProjectGithub))
          save.mutate(values)
      })}
      dirty={
        form.formState.isDirty &&
        !!projectId &&
        (!github || canUseProjectGithub)
      }
      saving={save.isPending}
      saveLabel="Save service"
      errors={{
        Name: form.formState.errors.name?.message,
        'HTTPS origin': form.formState.errors.origin?.message,
        'Allowed path prefixes': form.formState.errors.paths?.message,
        'Allowed HTTP methods': form.formState.errors.methods?.message,
        'Maximum access lifetime': form.formState.errors.maxTtl?.message,
        'Bearer credential': form.formState.errors.credential?.message,
        Repository: form.formState.errors.githubRepository?.message,
      }}
    >
      <fieldset disabled={save.isPending} className="space-y-4">
        {save.isError && (
          <Callout tone="error" title="Could not save service">
            {problemDetail(
              save.error,
              'Check your permissions and reload the service before trying again.'
            )}
          </Callout>
        )}
        <Field label="Service provider">
          {(props) => (
            <div
              {...props}
              role="group"
              className="grid grid-cols-1 gap-2 lg:grid-cols-3"
            >
              {[
                ...serviceProviders,
                {
                  id: 'custom' as const,
                  name: 'Custom HTTP service',
                  origin: '',
                },
              ].map((p) => (
                <button
                  key={p.id}
                  type="button"
                  aria-pressed={provider === p.id}
                  disabled={!!github}
                  className={`flex items-center gap-3 rounded-md border p-3 text-left text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${provider === p.id ? 'border-foreground bg-muted' : 'hover:bg-muted/50'}`}
                  onClick={() => {
                    form.setValue('provider', p.id, { shouldDirty: true })
                    form.setValue('origin', p.origin, { shouldDirty: true })
                  }}
                >
                  <CredentialProviderMark provider={p.id} />
                  <span>{p.name}</span>
                </button>
              ))}
            </div>
          )}
        </Field>
        <Field
          label="Credential source"
          description="Choose a stored bearer credential or an explicit repository from a GitHub connection. Credentials stay on the host."
        >
          {(props) => (
            <div
              {...props}
              role="group"
              className="flex flex-col gap-2 sm:flex-row sm:flex-wrap"
            >
              <Button
                type="button"
                variant={sourceMode === 'bearer' ? 'default' : 'outline'}
                onClick={() => {
                  if (save.isPending) return
                  setSourceMode('bearer')
                  form.setValue('githubRepository', undefined, {
                    shouldDirty: true,
                  })
                  form.setValue('credential', '')
                  form.setValue('clearCredential', false)
                }}
              >
                Bearer credential
              </Button>
              {(['api', 'git'] as const).map((protocol) => (
                <Button
                  key={protocol}
                  type="button"
                  variant={sourceMode === protocol ? 'default' : 'outline'}
                  disabled={!canUseProjectGithub}
                  onClick={() => {
                    setSourceMode(protocol)
                    form.setValue('credential', '')
                    form.setValue('clearCredential', false)
                    if (github) selectRepository({ ...github, protocol })
                  }}
                >
                  {protocol === 'api'
                    ? 'GitHub API reads'
                    : 'GitHub clone/fetch'}
                </Button>
              ))}
            </div>
          )}
        </Field>
        {!canUseProjectGithub && (
          <p className="text-sm text-muted-foreground">
            GitHub profiles require project write and Git repository read
            permissions.
          </p>
        )}
        {sourceMode !== 'bearer' && canUseProjectGithub && (
          <Field
            label="Repository"
            error={form.formState.errors.githubRepository?.message}
            description="Select the repository this service may access. Clone/fetch is read-only; push is not allowed."
          >
            {() => (
              <div className="space-y-3">
                {github && (
                  <p role="status">
                    Selected: {github.owner}/{github.repository}
                  </p>
                )}
                <VaultRepositoryPicker
                  connectionId={connectionId}
                  onConnectionChange={(id) => {
                    setConnectionId(id)
                    form.setValue('githubRepository', undefined, {
                      shouldDirty: true,
                    })
                  }}
                  onRepositorySelect={(repository) => {
                    if (repository.git_provider_connection_id !== connectionId)
                      return
                    selectRepository({
                      connection_id: repository.git_provider_connection_id,
                      owner: repository.owner,
                      repository: repository.name,
                      protocol: sourceMode,
                    })
                  }}
                />
              </div>
            )}
          </Field>
        )}
        <Field label="Name" error={form.formState.errors.name?.message}>
          {(props) => (
            <Input
              {...form.register('name')}
              {...props}
              placeholder="Repository API"
            />
          )}
        </Field>
        <Field
          label="HTTPS origin"
          error={form.formState.errors.origin?.message}
        >
          {(props) => (
            <Input
              {...form.register('origin')}
              readOnly={sourceMode !== 'bearer'}
              {...props}
              placeholder="https://api.example.com"
            />
          )}
        </Field>
        <Field
          label="Allowed path prefixes"
          error={form.formState.errors.paths?.message}
          description="One prefix per line. / allows all paths on this origin."
        >
          {(props) => (
            <Textarea
              {...form.register('paths')}
              readOnly={sourceMode !== 'bearer'}
              {...props}
              placeholder="/v1"
            />
          )}
        </Field>
        <Controller
          control={form.control}
          name="methods"
          render={({ field, fieldState }) => (
            <Field
              label="Allowed HTTP methods"
              error={fieldState.error?.message}
            >
              {(props) => (
                <div {...props} role="group" className="flex flex-wrap gap-4">
                  {(
                    ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE'] as const
                  ).map((method) => (
                    <label
                      key={method}
                      className="flex items-center gap-2 text-sm"
                    >
                      <Checkbox
                        disabled={!!github}
                        checked={field.value.includes(method)}
                        onCheckedChange={(checked) =>
                          field.onChange(
                            checked
                              ? [...field.value, method]
                              : field.value.filter((value) => value !== method)
                          )
                        }
                      />
                      {method}
                    </label>
                  ))}
                </div>
              )}
            </Field>
          )}
        />
        <Field
          label="Maximum access lifetime (seconds)"
          error={form.formState.errors.maxTtl?.message}
        >
          {(props) => (
            <Input
              {...form.register('maxTtl', { valueAsNumber: true })}
              {...props}
              type="number"
              min={60}
              max={86400}
            />
          )}
        </Field>
        {sourceMode === 'bearer' && (
          <>
            <Field
              label={
                service?.has_credential && !service.policy.github_repository
                  ? 'Replace bearer credential'
                  : 'Bearer credential'
              }
              error={form.formState.errors.credential?.message}
              description={
                service?.has_credential && !service.policy.github_repository
                  ? 'Leave empty to keep the stored credential. Enter a new value to replace it.'
                  : 'Required to enable access requests. Encrypted when saved; never displayed again.'
              }
            >
              {(props) => (
                <Input
                  {...form.register('credential')}
                  disabled={!!github}
                  {...props}
                  type="password"
                  autoComplete="new-password"
                />
              )}
            </Field>
            {service?.has_credential && !service.policy.github_repository && (
              <Controller
                control={form.control}
                name="clearCredential"
                render={({ field, fieldState }) => (
                  <Field
                    label="Remove stored credential"
                    error={fieldState.error?.message}
                  >
                    {(props) => (
                      <Checkbox
                        {...props}
                        checked={field.value}
                        onCheckedChange={(checked) =>
                          field.onChange(checked === true)
                        }
                      />
                    )}
                  </Field>
                )}
              />
            )}
          </>
        )}
        <Controller
          control={form.control}
          name="enabled"
          render={({ field, fieldState }) => (
            <Field
              label="Allow new access requests"
              error={fieldState.error?.message}
            >
              {(props) => (
                <Checkbox
                  {...props}
                  checked={field.value}
                  onCheckedChange={(checked) =>
                    field.onChange(checked === true)
                  }
                />
              )}
            </Field>
          )}
        />
        {service && (
          <p className="text-xs text-muted-foreground">
            Saving a change invalidates requests for the previous service
            version.
          </p>
        )}
        <p className="text-xs text-muted-foreground">
          Saving does not enable network access.
        </p>
      </fieldset>
    </Settings>
  )
}
