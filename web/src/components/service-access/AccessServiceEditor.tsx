// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Callout, Field, Settings } from '@temps-sdk/ds'
import { Input, Textarea, Checkbox, Button } from '@temps-sdk/ui'
import { toast } from 'sonner'
import {
  createAccessServiceGeneral,
  updateAccessServiceGeneral,
  type AccessServiceResponse,
  type ServiceCatalogEntry,
} from '@/api/client'
import { VaultConsumerPicker } from './VaultConsumerPicker'
import { VaultRepositoryPicker } from './VaultRepositoryPicker'
import { ServiceCredentialHelp } from './ServiceCredentialHelp'
import { CredentialProviderMark } from '@temps-sdk/ds'
import { ServiceProviderLogo } from './service-providers'
import { problemDetail } from '@/lib/api-problem'
import {
  serviceEditorSchema,
  serviceDefaults,
  serviceBody,
  isLogin,
  type AccessServiceValues,
} from './service-access-form'

export function AccessServiceEditor({
  projectId,
  catalogEntry,
  onChangeService,
  vaultId,
  service,
  canUseProjectGithub = false,
  onClose,
  onSaved,
}: {
  catalogEntry?: ServiceCatalogEntry
  onChangeService?: () => void
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
  const [sourceProjectId, setSourceProjectId] = useState<number | undefined>(
    service?.project_id ?? (projectId || undefined)
  )
  const [sourceMode, setSourceMode] = useState<'bearer' | 'api' | 'git'>(
    service?.policy.github_repository?.protocol ?? 'bearer'
  )
  const [connectionId, setConnectionId] = useState<number | null>(
    service?.policy.github_repository?.connection_id ?? null
  )
  const form = useForm<AccessServiceValues>({
    resolver: zodResolver(serviceEditorSchema(service, sourceMode)),
    defaultValues: service
      ? serviceDefaults(service)
      : {
          ...serviceDefaults(undefined, catalogEntry),
          name: catalogEntry?.id === 'custom' ? '' : (catalogEntry?.name ?? ''),
          provider: catalogEntry?.provider ?? 'custom',
          origin: catalogEntry?.origin ?? '',
        },
  })
  const github = useWatch({ control: form.control, name: 'githubRepository' })
  const delivery = useWatch({ control: form.control, name: 'delivery' })
  const login = isLogin(delivery)
  const reference = catalogEntry ?? service?.catalog ?? undefined
  const labels = reference?.credential_labels ?? []
  const defaultLabel = 'Bearer credential'
  const credentialLabel = login
    ? (labels[1] ?? 'Password')
    : (labels[0] ??
      (delivery?.format?.method === 'header' ? 'API key' : defaultLabel))
  const fixedHeaders = delivery?.headers ?? []
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
      if (values.githubRepository && !sourceProjectId)
        throw new Error('Choose the project that owns the GitHub connection')
      const body = {
        ...serviceBody(values, service?.version),
        vault_id: service ? undefined : vaultId,
      }
      if (service)
        return updateAccessServiceGeneral({
          path: { service_id: service.id },
          body,
          throwOnError: true,
        })
      return createAccessServiceGeneral({
        query: {
          source_project_id: values.githubRepository
            ? sourceProjectId
            : undefined,
        },
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
  // Only GitHub (and a custom service, which may point at it) can take a
  // repository from a connection; every other service just stores a credential.
  const offersGithubSource = service
    ? !!service.policy.github_repository ||
      service.policy.provider === 'github' ||
      service.policy.origin === 'https://github.com' ||
      !service.catalog
    : catalogEntry
      ? catalogEntry.id === 'github' || catalogEntry.id === 'custom'
      : true
  const heading = service
    ? 'Edit service'
    : (catalogEntry?.name ?? 'Add service')
  return (
    <Settings
      title={
        service || catalogEntry ? (
          <span className="flex items-center gap-3">
            <span aria-hidden="true">
              {service ? (
                <ServiceProviderLogo service={service} />
              ) : (
                <CredentialProviderMark
                  provider={catalogEntry?.logo ?? catalogEntry?.id}
                />
              )}
            </span>
            <span>{heading}</span>
          </span>
        ) : (
          heading
        )
      }
      headerActions={
        onChangeService && (
          <Button type="button" variant="outline" onClick={onChangeService}>
            Change service
          </Button>
        )
      }
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
        (!github || !!sourceProjectId) &&
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
        [credentialLabel]: form.formState.errors.credential?.message,
        Username: form.formState.errors.loginUsername?.message,
        'Request headers': form.formState.errors.delivery?.message,
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
        {sourceMode !== 'bearer' && (
          <Field
            label="GitHub source project"
            description="Only consumers in this project can use a vault containing this connection."
          >
            {() =>
              service ? (
                <p>
                  {sourceProjectId
                    ? `Project ${sourceProjectId}`
                    : 'Create a new service to select a GitHub source project.'}
                </p>
              ) : (
                <VaultConsumerPicker
                  kind="project"
                  selected={sourceProjectId}
                  onSelect={(id) => setSourceProjectId(Number(id))}
                />
              )
            }
          </Field>
        )}
        {offersGithubSource && (
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
        )}
        {offersGithubSource && !canUseProjectGithub && (
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
            {fixedHeaders.length > 0 && (
              <Field
                label="Request headers"
                error={form.formState.errors.delivery?.message}
                description="Sent with every request this service makes. Fixed text, not secret."
              >
                {() => (
                  <div className="space-y-2">
                    {fixedHeaders.map((header, index) => (
                      <div
                        key={header.name}
                        className="flex flex-col gap-1 sm:flex-row sm:items-center sm:gap-3"
                      >
                        <code className="text-sm sm:w-56 sm:shrink-0 [overflow-wrap:anywhere]">
                          {header.name}
                        </code>
                        <Input
                          aria-label={`${header.name} header value`}
                          value={header.value}
                          onChange={(event) =>
                            form.setValue(
                              'delivery',
                              {
                                ...(delivery ?? {
                                  format: { method: 'bearer' },
                                }),
                                headers: fixedHeaders.map((item, at) =>
                                  at === index
                                    ? { ...item, value: event.target.value }
                                    : item
                                ),
                              },
                              { shouldDirty: true, shouldValidate: true }
                            )
                          }
                        />
                      </div>
                    ))}
                  </div>
                )}
              </Field>
            )}
            {login && (
              <Field
                label={labels[0] ?? 'Username'}
                error={form.formState.errors.loginUsername?.message}
                description={
                  service?.has_credential
                    ? 'Enter the username and password together to replace the stored login.'
                    : 'Required with the password to enable access requests.'
                }
              >
                {(props) => (
                  <Input
                    {...form.register('loginUsername')}
                    {...props}
                    autoComplete="off"
                  />
                )}
              </Field>
            )}
            <Field
              label={
                service?.has_credential && !service.policy.github_repository
                  ? credentialLabel === defaultLabel
                    ? 'Replace bearer credential'
                    : `Replace ${credentialLabel}`
                  : credentialLabel
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
            {reference && reference.id !== 'custom' && !github && (
              <ServiceCredentialHelp entry={reference} />
            )}
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
