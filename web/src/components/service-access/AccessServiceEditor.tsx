// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Callout, Field, Settings } from '@temps-sdk/ds'
import { Input, Textarea, Checkbox, Button } from '@temps-sdk/ui'
import { toast } from 'sonner'
import {
  createAccessService,
  getProject,
  updateAccessService,
  type AccessServiceResponse,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import {
  accessServiceSchema,
  serviceDefaults,
  serviceBody,
  type AccessServiceValues,
} from './service-access-form'

export function AccessServiceEditor({
  projectId,
  service,
  canUseProjectGithub = false,
  onClose,
  onSaved,
}: {
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
  const form = useForm<AccessServiceValues>({
    resolver: zodResolver(accessServiceSchema),
    defaultValues: serviceDefaults(service),
  })
  const github = useWatch({ control: form.control, name: 'githubRepository' })
  const source = useMutation({
    mutationFn: async (protocol: 'api' | 'git') => {
      if (!projectId || !canUseProjectGithub)
        throw new Error(
          'Project write and Git repository read permissions are required'
        )
      const { data } = await getProject({
        path: { id: projectId },
        throwOnError: true,
      })
      if (
        !data.git_provider_connection_id ||
        !['github', 'github_app'].includes(data.git_provider_type ?? '') ||
        !data.repo_owner ||
        !data.repo_name
      ) {
        throw new Error('Connect this project to a GitHub repository first')
      }
      return {
        connection_id: data.git_provider_connection_id,
        owner: data.repo_owner,
        repository: data.repo_name,
        protocol,
      }
    },
    onSuccess: (reference) => {
      if (!mounted.current) return
      const repo = `${reference.owner}/${reference.repository}`
      form.setValue('githubRepository', reference, { shouldDirty: true })
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
      toast.success('Project GitHub connection selected')
    },
  })
  const save = useMutation({
    mutationFn: async (values: AccessServiceValues) => {
      if (!projectId)
        throw new Error('Select a project before saving a service')
      const body = serviceBody(values, service?.version)
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
        if (!save.isPending && !source.isPending) onClose()
      }}
      onSubmit={form.handleSubmit((values) => {
        if (
          !source.isPending &&
          !save.isPending &&
          (!github || canUseProjectGithub)
        )
          save.mutate(values)
      })}
      dirty={
        form.formState.isDirty &&
        !!projectId &&
        (!github || canUseProjectGithub)
      }
      saving={save.isPending || source.isPending}
      saveLabel="Save service"
      errors={{
        Name: form.formState.errors.name?.message,
        'HTTPS origin': form.formState.errors.origin?.message,
        'Allowed path prefixes': form.formState.errors.paths?.message,
        'Allowed HTTP methods': form.formState.errors.methods?.message,
        'Maximum access lifetime': form.formState.errors.maxTtl?.message,
        'Bearer credential': form.formState.errors.credential?.message,
      }}
    >
      <fieldset
        disabled={save.isPending || source.isPending}
        className="space-y-4"
      >
        {save.isError && (
          <Callout tone="error" title="Could not save service">
            {problemDetail(
              save.error,
              'Check your permissions and reload the service before trying again.'
            )}
          </Callout>
        )}
        <Field
          label="Credential source"
          description="GitHub uses this project’s connection on the host. No token is copied into the sandbox."
        >
          {(props) => (
            <div
              {...props}
              role="group"
              className="flex flex-col gap-2 sm:flex-row sm:flex-wrap"
            >
              <Button
                type="button"
                variant={!github ? 'default' : 'outline'}
                onClick={() => {
                  if (source.isPending || save.isPending) return
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
                  variant={
                    github?.protocol === protocol ? 'default' : 'outline'
                  }
                  disabled={!canUseProjectGithub}
                  onClick={() => {
                    if (!source.isPending && !save.isPending)
                      source.mutate(protocol)
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
        {source.isError && (
          <Callout tone="error" title="Could not use project GitHub connection">
            {problemDetail(
              source.error,
              'Connect this project to GitHub, then try again.'
            )}{' '}
            <a className="underline" href={`/projects/${projectId}`}>
              Open project
            </a>
          </Callout>
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
              readOnly={!!github}
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
              readOnly={!!github}
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
        <Field
          label={
            service?.has_credential
              ? 'Replace bearer credential'
              : 'Bearer credential (optional)'
          }
          error={form.formState.errors.credential?.message}
          description="Encrypted when saved; never displayed again. Leave empty to keep the existing credential."
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
                  disabled={!!github}
                  checked={field.value}
                  onCheckedChange={(checked) =>
                    field.onChange(checked === true)
                  }
                />
              )}
            </Field>
          )}
        />
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
