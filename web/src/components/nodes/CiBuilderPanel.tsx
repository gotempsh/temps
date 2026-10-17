// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { KeyRound, Timer } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Button, Field, SettingsGroup, SettingsSection } from '@temps-sdk/ds'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/ui/select'
import {
  getCiBuilderOptions,
  setCiBuilderMutation,
  provisionCiBuilderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { CiExecutorSettings } from '@/api/client/types.gen'
import { problemDetail } from '@/lib/api-problem'
import { toast } from 'sonner'

const executorSchema = z.object({
  owner: z.string().min(1),
  repository: z.string().min(1),
  organization: z.boolean(),
  git_ref: z.string().min(1),
  control_plane_url: z.url(),
  runner_archive_url: z.url(),
  runner_archive_sha256: z
    .string()
    .regex(/^[a-f0-9]{64}$/, 'Enter 64 lowercase hexadecimal characters'),
  image_repository: z.string().min(1),
  dispatch_token: z.string().nullable().optional(),
  registry_username: z.string(),
  registry_password: z.string(),
  identity: z.object({
    issuer: z.url(),
    jwks_url: z.url(),
    audience: z.string().min(1).max(256),
    subject: z.string().max(512),
  }),
  limits: z.object({
    concurrency: z.number().int().min(1).max(32),
    queue_wait_seconds: z.number().int().min(1).max(1800),
    build_timeout_seconds: z.number().int().min(1).max(14400),
  }),
})
type ExecutorForm = z.infer<typeof executorSchema>

const defaults: CiExecutorSettings = {
  owner: '',
  repository: 'temps-builder',
  organization: false,
  git_ref: 'main',
  control_plane_url: '',
  runner_archive_url: '',
  runner_archive_sha256: '',
  image_repository: '',
  identity: {
    issuer: 'https://token.actions.githubusercontent.com',
    jwks_url: 'https://token.actions.githubusercontent.com/.well-known/jwks',
    audience: 'temps-build',
    subject: '',
  },
  limits: {
    concurrency: 2,
    queue_wait_seconds: 300,
    build_timeout_seconds: 1800,
  },
}

export function CiBuilderPanel() {
  const query = useQuery({
    ...getCiBuilderOptions(),
    // Successful saves/provisioning refresh this form explicitly. Automatic
    // focus/reconnect refreshes must not discard edits in progress.
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
  return (
    <div id="ci-builder" className="space-y-4">
      <SettingsGroup
        title="CI builder"
        description="Build source deployments in GitHub Actions, then pull the verified image from your registry."
      >
        {query.isPending ? (
          <p role="status">Loading CI builder settings…</p>
        ) : query.isError ? (
          <div role="alert" className="space-y-2">
            <p>
              {problemDetail(
                query.error,
                'Could not load CI builder settings.'
              )}
            </p>
            <Button variant="outline" onClick={() => void query.refetch()}>
              Retry
            </Button>
          </div>
        ) : (
          query.data && (
            <CiBuilderForm
              key={query.dataUpdatedAt}
              initial={query.data.settings ?? defaults}
              configured={query.data.configured}
              saved={query.data.settings != null}
              reason={query.data.reason}
            />
          )
        )}
      </SettingsGroup>
    </div>
  )
}
function CiBuilderForm({
  initial,
  configured,
  saved,
  reason,
}: {
  initial: CiExecutorSettings
  configured: boolean
  saved: boolean
  reason?: string | null
}) {
  const form = useForm<ExecutorForm>({
    resolver: zodResolver(executorSchema),
    defaultValues: {
      ...initial,
      dispatch_token: null,
      registry_username: '',
      registry_password: '',
    },
  })
  const values = form.watch()
  const registryUser = values.registry_username
  const registryPassword = values.registry_password
  const queryClient = useQueryClient()
  const save = useMutation({
    ...setCiBuilderMutation(),
    onSuccess: async () => {
      toast.success('CI builder settings saved')
      await queryClient.invalidateQueries({
        queryKey: getCiBuilderOptions().queryKey,
      })
    },
  })
  const provision = useMutation({
    ...provisionCiBuilderMutation(),
    onSuccess: async () => {
      toast.success('Builder repository and CI workflow provisioned')
      await queryClient.invalidateQueries({
        queryKey: getCiBuilderOptions().queryKey,
      })
    },
  })
  const dirty = form.formState.isDirty
  return (
    <form
      className="space-y-5"
      onSubmit={form.handleSubmit((submitted) => {
        if (save.isPending || provision.isPending) return
        const { registry_username, registry_password, ...settings } = submitted
        save.mutate({
          body: {
            ...settings,
            dispatch_token: settings.dispatch_token || null,
            registry_credentials:
              registry_username || registry_password
                ? { username: registry_username, password: registry_password }
                : null,
          },
        })
      })}
    >
      {!configured && (
        <p role="status" className="text-sm text-muted-foreground">
          {reason} For example, build a preview without using this server’s
          build CPU. Save the settings below, then provision the repository.
        </p>
      )}
      {(
        [
          ['owner', 'Repository owner', 'Account or organization name'],
          [
            'repository',
            'Builder repository',
            'One dedicated builder repository for this installation',
          ],
          [
            'git_ref',
            'Builder branch',
            'The workflow must exist on the default branch',
          ],
          [
            'control_plane_url',
            'Public Temps URL',
            'HTTPS URL reachable from CI, for example https://temps.example.com',
          ],
          [
            'image_repository',
            'Registry repository',
            'For example registry.example.com/builds/app (without a tag)',
          ],
          [
            'runner_archive_url',
            'Runner release archive URL',
            'Pinned HTTPS URL for a Temps Linux amd64 release containing the runner command',
          ],
          [
            'runner_archive_sha256',
            'Runner archive SHA-256',
            '64 lowercase hexadecimal characters',
          ],
        ] as const
      ).map(([key, label, description]) => (
        <Field
          key={key}
          label={label}
          description={description}
          error={form.formState.errors[key]?.message}
        >
          {(props) => (
            <Input
              {...props}
              readOnly={save.isPending || provision.isPending}
              required
              {...form.register(key)}
              value={values[key]}
              onChange={(event) =>
                form.setValue(key, event.target.value, {
                  shouldDirty: true,
                  shouldValidate: true,
                })
              }
            />
          )}
        </Field>
      ))}
      <Field
        label="Repository API token"
        description={
          configured
            ? 'Leave empty to keep the saved token. Requires repository contents, workflows, and Actions write permissions.'
            : 'Requires repository creation, contents, workflows, and Actions write permissions.'
        }
      >
        {(props) => (
          <Input
            {...props}
            readOnly={save.isPending || provision.isPending}
            type="password"
            autoComplete="new-password"
            required={!saved}
            {...form.register('dispatch_token')}
            value={values.dispatch_token ?? ''}
            onChange={(event) =>
              form.setValue('dispatch_token', event.target.value, {
                shouldDirty: true,
                shouldValidate: true,
              })
            }
          />
        )}
      </Field>
      <Field label="Owner type">
        {(props) => (
          <Select
            value={values.organization ? 'organization' : 'account'}
            disabled={save.isPending || provision.isPending}
            onValueChange={(value) =>
              form.setValue('organization', value === 'organization', {
                shouldDirty: true,
              })
            }
          >
            <SelectTrigger {...props}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="account">Account</SelectItem>
              <SelectItem value="organization">Organization</SelectItem>
            </SelectContent>
          </Select>
        )}
      </Field>
      <SettingsSection
        title="Runner identity and registry credentials"
        icon={KeyRound}
        defaultOpen={!configured}
      >
        <div className="space-y-4">
          {(['issuer', 'jwks_url', 'audience', 'subject'] as const).map(
            (key) => (
              <Field
                key={key}
                error={form.formState.errors.identity?.[key]?.message}
                label={
                  {
                    issuer: 'OIDC issuer',
                    jwks_url: 'Signing keys URL',
                    audience: 'Audience',
                    subject: 'Exact OIDC subject',
                  }[key]
                }
                description={
                  key === 'subject'
                    ? 'Leave empty to provision and pin the repository’s immutable ID, or enter the exact subject for an existing trust policy.'
                    : undefined
                }
              >
                {(props) => (
                  <Input
                    {...props}
                    readOnly={save.isPending || provision.isPending}
                    required={key !== 'subject'}
                    {...form.register(`identity.${key}`)}
                    value={values.identity[key]}
                    onChange={(event) =>
                      form.setValue(`identity.${key}`, event.target.value, {
                        shouldDirty: true,
                        shouldValidate: true,
                      })
                    }
                  />
                )}
              </Field>
            )
          )}
          <Field
            label="Registry username"
            optional
            description="Use credentials scoped to this build repository. Leave both fields empty to preserve saved credentials."
          >
            {(props) => (
              <Input
                {...props}
                readOnly={save.isPending || provision.isPending}
                {...form.register('registry_username')}
                value={registryUser}
                autoComplete="off"
                onChange={(event) =>
                  form.setValue('registry_username', event.target.value, {
                    shouldDirty: true,
                  })
                }
              />
            )}
          </Field>
          <Field label="Registry password or token" optional>
            {(props) => (
              <Input
                {...props}
                readOnly={save.isPending || provision.isPending}
                type="password"
                autoComplete="new-password"
                {...form.register('registry_password')}
                value={registryPassword}
                onChange={(event) =>
                  form.setValue('registry_password', event.target.value, {
                    shouldDirty: true,
                  })
                }
              />
            )}
          </Field>
        </div>
      </SettingsSection>
      <SettingsSection title="CI build limits" icon={Timer}>
        <div className="space-y-4">
          {(
            [
              ['concurrency', 'Concurrent CI builds', 32],
              ['queue_wait_seconds', 'Queue wait (seconds)', 1800],
              ['build_timeout_seconds', 'Build timeout (seconds)', 14400],
            ] as const
          ).map(([key, label, max]) => (
            <Field
              key={key}
              label={label}
              description={`1–${max}`}
              error={form.formState.errors.limits?.[key]?.message}
            >
              {(props) => (
                <Input
                  {...props}
                  readOnly={save.isPending || provision.isPending}
                  required
                  type="number"
                  min={1}
                  max={max}
                  value={values.limits[key]}
                  onChange={(event) =>
                    form.setValue(`limits.${key}`, Number(event.target.value), {
                      shouldDirty: true,
                      shouldValidate: true,
                    })
                  }
                />
              )}
            </Field>
          ))}
        </div>
      </SettingsSection>
      <p className="text-sm text-muted-foreground">
        CI builds fail if identity, platform, or digest verification is
        rejected. They do not fall back to a local build. Scans remain optional.
        Set a project’s Build location to CI after provisioning.
      </p>
      {save.isError && (
        <p role="alert" className="text-sm text-destructive">
          {problemDetail(
            save.error,
            'Could not save CI builder settings. Your input is retained.'
          )}
        </p>
      )}
      {provision.isError && (
        <p role="alert" className="text-sm text-destructive">
          {problemDetail(
            provision.error,
            'Could not provision the builder repository. Check API token permissions and retry.'
          )}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button
          type="submit"
          busy={save.isPending}
          busyLabel="Saving…"
          aria-disabled={provision.isPending}
          className={
            provision.isPending ? 'pointer-events-none opacity-50' : undefined
          }
        >
          Save CI builder
        </Button>
        <Button
          type="button"
          variant="outline"
          aria-disabled={!saved || dirty || save.isPending}
          className={
            !saved || dirty || save.isPending
              ? 'pointer-events-none opacity-50'
              : undefined
          }
          busy={provision.isPending}
          busyLabel="Provisioning…"
          onClick={() => {
            if (saved && !dirty && !provision.isPending && !save.isPending)
              provision.mutate({})
          }}
        >
          Provision builder repository
        </Button>
      </div>
      {dirty && saved && (
        <p className="text-sm text-muted-foreground">
          Save changes before provisioning.
        </p>
      )}
    </form>
  )
}
