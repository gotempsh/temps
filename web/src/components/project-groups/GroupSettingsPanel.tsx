// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { toast } from 'sonner'
import {
  AlertTriangle,
  Boxes,
  FolderGit2,
  FolderPen,
  GitBranch,
  Settings2,
  Trash2,
  Waypoints,
  type LucideIcon,
} from 'lucide-react'
import {
  deleteProjectGroupMutation,
  listConnectionsOptions,
  listGitProvidersOptions,
  listGroupEnvVarsOptions,
  removeProjectFromGroupMutation,
  updateProjectGroupMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectGroupResponse } from '@/api/client'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import { PageHeader } from '@/components/layout/PageContainer'
import { SaveRow } from '@temps-sdk/ds'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { SettingsSection } from '@/components/ui/settings-section'
import { SectionNavLayout } from '@/components/layout/SectionNavLayout'
import { getErrorMessage } from '@/utils/errorHandling'
import { groupHref, serviceCount } from '@/lib/terminology'
import { useInvalidateGroups } from './useProjectGroups'
import { DisconnectRepositoryDialog } from './DisconnectRepositoryDialog'
import { GroupRoutesSettings } from './GroupRoutesSettings'
import { ServicePresetIcon } from './ServicePresetIcon'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { projectPresetLabel } from '@/components/dashboard/project-card-data'

type SettingsPage = 'general' | 'repository' | 'routes' | 'services'

/** Same shape as a service's settings: a page per concern, beside a nav. */
const SETTINGS_PAGES: { key: SettingsPage; title: string; icon: LucideIcon }[] =
  [
    { key: 'general', title: 'General', icon: Settings2 },
    { key: 'repository', title: 'Git repository', icon: FolderGit2 },
    { key: 'routes', title: 'Path routes', icon: Waypoints },
    { key: 'services', title: 'Services', icon: Boxes },
  ]

interface GroupSettingsPanelProps {
  group: ProjectGroupResponse
  /** Opens the dialog that connects or changes the project's repository. */
  onConnectRepository: () => void
}

export function GroupSettingsPanel({
  group,
  onConnectRepository,
}: GroupSettingsPanelProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const requested = searchParams.get('section')
  const page: SettingsPage =
    SETTINGS_PAGES.find((p) => p.key === requested)?.key ?? 'general'
  const invalidate = useInvalidateGroups()
  const [removing, setRemoving] = useState<{ id: number; name: string } | null>(
    null
  )
  const [confirmDelete, setConfirmDelete] = useState(false)

  const vars = useQuery({
    ...listGroupEnvVarsOptions({ path: { group_id: group.id } }),
    retry: false,
  })
  const varCount = vars.data?.length ?? 0

  const remove = useMutation({
    ...removeProjectFromGroupMutation(),
    meta: { errorTitle: 'Could not remove the service from the project' },
  })
  const destroy = useMutation({
    ...deleteProjectGroupMutation(),
    meta: { errorTitle: 'The project could not be deleted' },
  })

  const removeService = async () => {
    if (!removing) return
    try {
      await remove.mutateAsync({
        path: { group_id: group.id, project_id: removing.id },
      })
      await invalidate()
      toast.success(`${removing.name} is now a standalone service`)
    } catch {
      // The app-wide mutation handler already toasts this failure.
    } finally {
      setRemoving(null)
    }
  }

  const deleteGroup = async () => {
    try {
      await destroy.mutateAsync({ path: { group_id: group.id } })
      await invalidate()
      toast.success(
        `Project “${group.name}” deleted; its services are standalone now`
      )
      navigate('/projects')
    } catch {
      // The app-wide mutation handler already toasts this failure.
      setConfirmDelete(false)
    }
  }

  // Follows the server's values (a save, another tab, a refetch): a new set
  // of them remounts the form, discarding unsaved edits.
  const identity = (
    <GroupIdentityForm
      key={[group.name, group.slug, group.description ?? ''].join('\u0000')}
      group={group}
    />
  )

  const deleteConsequences = `${
    group.projects.length > 0
      ? `Its ${serviceCount(group.projects.length)} keep running and become standalone. `
      : ''
  }${
    varCount > 0
      ? `Its ${varCount} shared ${varCount === 1 ? 'variable is' : 'variables are'} deleted, and services stop receiving ${varCount === 1 ? 'it' : 'them'} on their next deployment.`
      : 'It has no shared variables.'
  }`

  const deletion = (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        {group.projects.length > 0
          ? `Its ${serviceCount(group.projects.length)} keep running as standalone services; no service is redeployed or deleted.`
          : 'It has no services.'}{' '}
        {varCount > 0
          ? `Its ${varCount} shared ${varCount === 1 ? 'variable is' : 'variables are'} deleted.`
          : ''}
      </p>
      <Button variant="destructive" onClick={() => setConfirmDelete(true)}>
        Delete project
      </Button>
    </div>
  )

  const services = (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        Removing a service makes it standalone. It keeps running, and stops
        receiving this project&apos;s shared variables on its next deployment.
      </p>
      {group.projects.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No services in this project.
        </p>
      ) : (
        <div className="divide-y rounded-lg border">
          {group.projects.map((p) => (
            <div key={p.id} className="flex items-center gap-3 px-3 py-2">
              <ServiceIcon service={p} size="sm" />
              <Link
                to={`/projects/${p.slug}`}
                className="min-w-0 flex-1 truncate text-sm font-medium hover:underline"
              >
                {p.name}
              </Link>
              <span className="hidden shrink-0 items-center gap-1.5 text-xs text-muted-foreground sm:flex">
                <ServicePresetIcon
                  preset={p.preset}
                  className="size-4 rounded"
                />
                {projectPresetLabel(p.preset)}
              </span>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setRemoving({ id: p.id, name: p.name })}
              >
                Remove
              </Button>
            </div>
          ))}
        </div>
      )}
    </div>
  )

  const content: Record<SettingsPage, ReactNode> = {
    general: (
      <>
        <SettingsSection title="Project identity" icon={FolderPen} defaultOpen>
          {identity}
        </SettingsSection>
        <SettingsSection title="Delete project" icon={Trash2}>
          {deletion}
        </SettingsSection>
      </>
    ),
    repository: (
      <SettingsSection title="Repository" icon={FolderGit2} defaultOpen>
        <RepositorySettings group={group} onConnect={onConnectRepository} />
      </SettingsSection>
    ),
    routes: (
      <SettingsSection title="Path routes" icon={Waypoints} defaultOpen>
        <GroupRoutesSettings group={group} />
      </SettingsSection>
    ),
    services: (
      <SettingsSection
        title={`Services (${group.projects.length})`}
        icon={Boxes}
        defaultOpen
      >
        {services}
      </SettingsSection>
    ),
  }

  return (
    <SectionNavLayout
      title="Settings"
      links={SETTINGS_PAGES.map((p) => ({
        key: p.key,
        title: p.title,
        icon: p.icon,
        to: { search: `?tab=settings&section=${p.key}` },
        active: p.key === page,
      }))}
    >
      <div className="min-w-0 space-y-4">
        <PageHeader title={SETTINGS_PAGES.find((p) => p.key === page)?.title} />
        {/* Keyed so each page opens with its own sections' default state. */}
        <div key={page} className="space-y-4">
          {content[page]}
        </div>
      </div>

      <ConfirmDeleteDialog
        open={removing !== null}
        onOpenChange={(open) => {
          if (!open) setRemoving(null)
        }}
        resourceType="service"
        resourceName={removing?.name ?? ''}
        verb="Remove"
        confirmLabel="Remove from project"
        consequences={`It leaves ${group.name}, becomes a standalone service and stops receiving this project's shared variables on its next deployment.`}
        irreversible={false}
        isPending={remove.isPending}
        onConfirm={() => void removeService()}
      />

      <ConfirmDeleteDialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        resourceType="project"
        resourceName={group.name}
        consequences={deleteConsequences}
        isPending={destroy.isPending}
        onConfirm={() => void deleteGroup()}
      />
    </SectionNavLayout>
  )
}

const identitySchema = z.object({
  name: z.string().trim().min(1, 'Give the project a name.'),
  slug: z.string().trim().min(1, 'Give the project an address.'),
  description: z.string().trim(),
})

type IdentityValues = z.infer<typeof identitySchema>

/** Name, address and description of the project. */
function GroupIdentityForm({ group }: { group: ProjectGroupResponse }) {
  const navigate = useNavigate()
  const invalidate = useInvalidateGroups()
  const update = useMutation({ ...updateProjectGroupMutation() })

  const form = useForm<IdentityValues>({
    resolver: zodResolver(identitySchema),
    defaultValues: {
      name: group.name,
      slug: group.slug,
      description: group.description ?? '',
    },
  })
  const [name, slug, description] = useWatch({
    control: form.control,
    name: ['name', 'slug', 'description'],
  })
  const dirty =
    name.trim() !== group.name ||
    slug.trim() !== group.slug ||
    description.trim() !== (group.description ?? '')

  const save = async (values: IdentityValues) => {
    try {
      const updated = await update.mutateAsync({
        path: { group_id: group.id },
        body: {
          name: values.name,
          slug: values.slug,
          description: values.description,
        },
      })
      await invalidate()
      toast.success('Project updated')
      if (updated.slug !== group.slug) {
        navigate(`${groupHref(updated.slug)}?tab=settings&section=general`, {
          replace: true,
        })
      }
    } catch (err) {
      form.setError('root', {
        type: 'server',
        message: getErrorMessage(err, 'The project could not be updated.'),
      })
    }
  }

  return (
    <Form {...form}>
      <form
        onSubmit={(event) => void form.handleSubmit(save)(event)}
        className="max-w-2xl space-y-4"
      >
        <p className="text-sm text-muted-foreground">
          Renaming changes this project&apos;s address; its services keep their
          own names and URLs.
        </p>
        <FormField
          control={form.control}
          name="name"
          render={({ field }) => (
            <FormItem>
              <FormLabel htmlFor="group-settings-name">Name</FormLabel>
              <FormControl>
                <Input {...field} id="group-settings-name" />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
        <FormField
          control={form.control}
          name="slug"
          render={({ field }) => (
            <FormItem>
              <FormLabel htmlFor="group-settings-slug">Slug</FormLabel>
              <FormControl>
                <Input
                  {...field}
                  id="group-settings-slug"
                  className="font-mono"
                  onChange={(e) => field.onChange(e.target.value.toLowerCase())}
                />
              </FormControl>
              <FormDescription className="text-xs">
                Used in the console address:{' '}
                {groupHref(slug.trim() || group.slug)}
              </FormDescription>
              <FormMessage />
            </FormItem>
          )}
        />
        <FormField
          control={form.control}
          name="description"
          render={({ field }) => (
            <FormItem>
              <FormLabel htmlFor="group-settings-description">
                Description
              </FormLabel>
              <FormControl>
                <Textarea {...field} id="group-settings-description" rows={2} />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
        {form.formState.errors.root && (
          <p role="alert" className="text-sm font-medium text-destructive">
            {form.formState.errors.root.message}
          </p>
        )}
        <SaveRow dirty={dirty} saving={update.isPending} />
      </form>
    </Form>
  )
}

/**
 * Where the project's services deploy from. Connecting, changing and
 * disconnecting live here with the rest of the configuration; the overview
 * shows how each service relates to it.
 */
function RepositorySettings({
  group,
  onConnect,
}: {
  group: ProjectGroupResponse
  onConnect: () => void
}) {
  const repo = group.repository
  const [disconnecting, setDisconnecting] = useState(false)
  const connectionId = repo?.git_provider_connection_id ?? null
  const connections = useQuery({
    ...listConnectionsOptions(),
    enabled: connectionId !== null,
  })
  const providers = useQuery({
    ...listGitProvidersOptions(),
    enabled: connectionId !== null,
  })
  const connection = (connections.data?.connections ?? []).find(
    (c) => c.id === connectionId
  )
  const provider = (providers.data ?? []).find(
    (p) => p.id === connection?.provider_id
  )
  const followers = group.projects.filter((p) => p.inherits_repository).length

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        Services that follow it deploy from this repository and branch, each
        from its own directory. Changing it here updates all of them.
      </p>
      {repo ? (
        <div className="rounded-lg border">
          <div className="flex flex-wrap items-center gap-3 px-3 py-3">
            {repo.public_provider ? (
              <ProviderLogo
                providerType={repo.public_provider}
                className="size-5 shrink-0"
              />
            ) : provider ? (
              <ProviderLogo
                providerType={provider.provider_type}
                className="size-5 shrink-0"
              />
            ) : (
              <FolderGit2
                className="size-5 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
            )}
            <div className="min-w-0 flex-1">
              <p className="truncate font-mono text-sm font-medium">
                {repo.repo_owner}/{repo.repo_name}
              </p>
              <p className="flex items-center gap-1 text-xs text-muted-foreground">
                <GitBranch className="size-3" aria-hidden="true" />
                <span className="font-mono">{repo.main_branch}</span>
                {repo.public_provider && (
                  <span className="truncate">
                    · public, deploys when you start them
                  </span>
                )}
                {connection && (
                  <span className="truncate">
                    · via {connection.account_name}
                    {provider ? ` on ${provider.name}` : ''}
                  </span>
                )}
              </p>
            </div>
            <Button variant="outline" size="sm" onClick={onConnect}>
              Change
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setDisconnecting(true)}
            >
              Disconnect
            </Button>
          </div>
          {connectionId === null && !repo.public_provider && (
            <div
              role="alert"
              className="flex items-center gap-2 border-t bg-destructive/5 px-3 py-2 text-sm"
            >
              <AlertTriangle className="size-4 shrink-0 text-destructive" />
              <p className="flex-1">
                Its git connection was removed, so services cannot be updated
                from here. Choose Change to connect it again.
              </p>
            </div>
          )}
          <p className="border-t px-3 py-2 text-xs text-muted-foreground">
            {followers === 0
              ? 'No service follows it yet.'
              : `${followers} of ${serviceCount(group.projects.length)} follow it.`}{' '}
            <Link
              to={groupHref(group.slug)}
              className="font-medium text-foreground hover:underline"
            >
              See each service
            </Link>
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-3 rounded-lg border border-dashed px-4 py-4 sm:flex-row sm:items-center">
          <p className="flex-1 text-sm text-muted-foreground">
            No repository connected. Using a monorepo? Connect it to add its
            apps (say <code>apps/web</code> and <code>apps/api</code>) as
            services and change their branch from one place.
          </p>
          <Button size="sm" className="shrink-0" onClick={onConnect}>
            Connect repository
          </Button>
        </div>
      )}
      <DisconnectRepositoryDialog
        group={group}
        open={disconnecting}
        onOpenChange={setDisconnecting}
      />
    </div>
  )
}
