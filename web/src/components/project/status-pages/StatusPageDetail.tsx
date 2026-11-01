// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deleteStatusPageMutation,
  getStatusPageOptions,
  listStatusPageDomainsOptions,
  publishStatusPageMutation,
  rotateStatusPageUrlMutation,
  unpublishStatusPageMutation,
  updateStatusPageMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { StatusPageResponse } from '@/api/client'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Form, FormField, FormItem } from '@/components/ui/form'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { cn } from '@/lib/utils'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Boxes,
  CheckCircle2,
  Circle,
  ExternalLink,
  Eye,
  EyeOff,
  Globe,
  Home,
  MoreVertical,
  Palette,
  RefreshCw,
  Siren,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { StatusPageProjects, StatusPageStateBadge } from './ProjectStatusPages'
import { StatusPageComponentsEditor } from './StatusPageComponentsEditor'
import { StatusPageDomains } from './StatusPageDomains'
import { PublicLinkSection } from './StatusPagePublicLink'
import { StatusPageAppearanceForm } from './StatusPageAppearanceForm'
import { StatusPageIncidents } from './StatusPageIncidents'
import { StatusPagePreview } from './StatusPagePreview'
import {
  formDefaults,
  operationQueries,
  statusPageFormSchema,
  toSelection,
  type StatusPageFormValues,
} from './status-page-form'

const DETAIL_TABS = [
  'overview',
  'components',
  'appearance',
  'incidents',
  'domains',
] as const
type DetailTab = (typeof DETAIL_TABS)[number]

function isCustomized(page: StatusPageResponse) {
  const a = page.appearance
  return (
    a.theme !== 'auto' ||
    Boolean(a.accent_color || a.logo_url || a.website_url || page.description)
  )
}

interface ChecklistStep {
  done: boolean
  title: string
  detail: string
  optional?: boolean
  action?: { label: string; onClick: () => void; disabled?: boolean }
}

/**
 * Where the page stands and the next thing to do, so a new page is never a
 * wall of forms. Every step links to the tab that completes it.
 */
function SetupChecklist({
  page,
  onGoTo,
  onPublish,
  publishing,
}: {
  page: StatusPageResponse
  onGoTo: (tab: DetailTab) => void
  onPublish: () => void
  publishing: boolean
}) {
  const domains = useQuery(
    listStatusPageDomainsOptions({ path: { status_page_id: page.id } })
  )
  const hasComponents = page.monitors.length > 0
  const hasDomain = (domains.data?.length ?? 0) > 0
  const steps: ChecklistStep[] = [
    {
      done: hasComponents,
      title: 'Choose components',
      detail: hasComponents
        ? `${page.monitors.length} shown: ${page.monitors.map((m) => m.display_name).join(', ')}`
        : 'Pick the environments or monitors visitors should see.',
      action: { label: 'Edit components', onClick: () => onGoTo('components') },
    },
    {
      done: isCustomized(page),
      optional: true,
      title: 'Make it yours',
      detail:
        'Logo, brand color, theme, and whether to show uptime and response times.',
      action: { label: 'Customize', onClick: () => onGoTo('appearance') },
    },
    {
      done: page.is_published,
      title: 'Publish',
      detail: page.is_published
        ? 'Live at the public link below.'
        : 'Nothing is public until you publish.',
      action: page.is_published
        ? undefined
        : {
            label: publishing ? 'Publishing...' : 'Publish',
            onClick: onPublish,
            disabled: !hasComponents || publishing,
          },
    },
    {
      done: hasDomain,
      optional: true,
      title: 'Use your own domain',
      detail: 'Serve it at an address such as status.example.com.',
      action: { label: 'Add domain', onClick: () => onGoTo('domains') },
    },
  ]
  const remaining = steps.filter((s) => !s.done && !s.optional).length
  return (
    <Card>
      <CardContent className="space-y-1 p-4">
        <div className="pb-2 text-sm font-medium">
          {remaining === 0
            ? 'Your status page is live.'
            : `${remaining} step${remaining === 1 ? '' : 's'} to go live`}
        </div>
        <ol className="divide-y">
          {steps.map((step) => (
            <li
              key={step.title}
              className="flex flex-col gap-2 py-3 sm:flex-row sm:items-center sm:justify-between"
            >
              <div className="flex min-w-0 items-start gap-3">
                {step.done ? (
                  <CheckCircle2 className="mt-0.5 size-5 shrink-0 text-success" />
                ) : (
                  <Circle className="mt-0.5 size-5 shrink-0 text-muted-foreground" />
                )}
                <div className="min-w-0">
                  <div
                    className={cn(
                      'text-sm font-medium',
                      step.done && 'text-muted-foreground'
                    )}
                  >
                    {step.title}
                    {step.optional && (
                      <span className="ml-2 text-xs font-normal text-muted-foreground">
                        Optional
                      </span>
                    )}
                  </div>
                  <p className="truncate text-sm text-muted-foreground">
                    {step.detail}
                  </p>
                </div>
              </div>
              {step.action && (
                <Button
                  type="button"
                  size="sm"
                  variant={step.done ? 'ghost' : 'outline'}
                  disabled={step.action.disabled}
                  onClick={step.action.onClick}
                  className="shrink-0 self-start sm:self-auto"
                >
                  {step.action.label}
                </Button>
              )}
            </li>
          ))}
        </ol>
      </CardContent>
    </Card>
  )
}

function DetailSkeleton() {
  return (
    <div className="w-full min-w-0 space-y-6">
      <Skeleton className="h-8 w-64" />
      <Skeleton className="h-20 w-full" />
      <Skeleton className="h-64 w-full" />
    </div>
  )
}

function StatusPageForm({ page }: { page: StatusPageResponse }) {
  const queryClient = useQueryClient()
  const form = useForm<StatusPageFormValues>({
    resolver: zodResolver(statusPageFormSchema),
    defaultValues: formDefaults(page),
  })

  // Publishing, unpublishing or replacing the link refetches the page. Take
  // the fresh server values for fields the operator has not touched, but
  // never discard unsaved edits.
  useEffect(() => {
    form.reset(formDefaults(page), { keepDirtyValues: true })
  }, [form, page])

  const update = useMutation({
    ...updateStatusPageMutation(),
    meta: { errorTitle: 'Failed to save status page' },
    onSuccess: async (saved) => {
      // The saved page is the new clean baseline. The form is disabled while
      // a save is pending, so there are no newer edits for this to discard.
      form.reset(formDefaults(saved))
      await queryClient.invalidateQueries(operationQueries('getStatusPage'))
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      await queryClient.invalidateQueries(operationQueries('previewStatusPage'))
      toast.success(
        page.is_published
          ? 'Saved. The public page is updated.'
          : 'Saved. Publish when you are ready.'
      )
    },
  })

  const onSubmit = (values: StatusPageFormValues) => {
    update.mutate({
      path: { status_page_id: page.id },
      body: { monitors: toSelection(values.components) },
    })
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)}>
        <fieldset
          className="min-w-0 space-y-6"
          disabled={update.isPending}
          aria-busy={update.isPending}
        >
          <section className="space-y-3">
            <div>
              <h3 className="text-lg font-semibold">Components</h3>
              <p className="text-sm text-muted-foreground">
                Each row is one component on the page, from any of your
                projects. Components with the same group are shown together
                under that heading. Only the public name, its current state,
                daily uptime and average response time are published — never the
                monitor&apos;s internal name, URL, host or error messages.
              </p>
            </div>
            <FormField
              control={form.control}
              name="components"
              render={({ field, fieldState }) => (
                <FormItem>
                  <StatusPageComponentsEditor
                    value={field.value}
                    onChange={field.onChange}
                    errors={form.formState.errors.components}
                    initialProjectId={page.projects[0]?.id}
                  />
                  {fieldState.error?.message && (
                    <p className="text-sm font-medium text-destructive">
                      {fieldState.error.message}
                    </p>
                  )}
                </FormItem>
              )}
            />
          </section>

          <div className="flex justify-end">
            <Button type="submit" disabled={update.isPending}>
              {update.isPending ? 'Saving...' : 'Save changes'}
            </Button>
          </div>
        </fieldset>
      </form>
    </Form>
  )
}

export function StatusPageDetail() {
  const { statusPageId } = useParams()
  const id = Number(statusPageId)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [confirmRotate, setConfirmRotate] = useState(false)
  const [searchParams, setSearchParams] = useSearchParams()
  const tab = DETAIL_TABS.includes(searchParams.get('tab') as DetailTab)
    ? (searchParams.get('tab') as DetailTab)
    : 'overview'
  const setTab = (next: string) => {
    const params = new URLSearchParams(searchParams)
    if (next === 'overview') params.delete('tab')
    else params.set('tab', next)
    setSearchParams(params, { replace: true })
  }

  const pageQuery = useQuery({
    ...getStatusPageOptions({ path: { status_page_id: id } }),
    enabled: Number.isFinite(id),
  })

  const refresh = async () => {
    await queryClient.invalidateQueries(operationQueries('getStatusPage'))
    await queryClient.invalidateQueries(operationQueries('listStatusPages'))
    // Unpublishing releases every custom domain's certificate, so their
    // state changes too; a stale "Serving" row would hide Verify.
    await queryClient.invalidateQueries(
      operationQueries('listStatusPageDomains')
    )
  }
  const publish = useMutation({
    ...publishStatusPageMutation(),
    meta: { errorTitle: 'Failed to publish status page' },
    onSuccess: async () => {
      await refresh()
      await queryClient.invalidateQueries(operationQueries('previewStatusPage'))
      toast.success('Published. The public link is live.')
    },
  })
  const unpublish = useMutation({
    ...unpublishStatusPageMutation(),
    meta: { errorTitle: 'Failed to unpublish status page' },
    onSuccess: async () => {
      await refresh()
      toast.success('Unpublished. The public link now returns “not found”.')
    },
  })
  const rotate = useMutation({
    ...rotateStatusPageUrlMutation(),
    meta: { errorTitle: 'Failed to replace the public link' },
    onSuccess: async () => {
      await refresh()
      setConfirmRotate(false)
      toast.success('New public link created. The old one no longer works.')
    },
  })
  const remove = useMutation({
    ...deleteStatusPageMutation(),
    meta: { errorTitle: 'Failed to delete status page' },
    onSuccess: async () => {
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      toast.success('Status page deleted.')
      navigate('/status-pages')
    },
  })

  const { setBreadcrumbs } = useBreadcrumbs()
  const title = pageQuery.data?.title
  usePageTitle(title ? `${title} · Status pages` : 'Status page')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Status pages', href: '/status-pages' },
      { label: title ?? 'Status page' },
    ])
  }, [setBreadcrumbs, title])

  if (pageQuery.isError) {
    const error = pageQuery.error
    return (
      <ErrorAlert
        title="Failed to load status page"
        description={
          error instanceof Error
            ? error.message
            : 'An unexpected error occurred'
        }
        retry={() => pageQuery.refetch()}
      />
    )
  }
  if (!pageQuery.data) {
    return <DetailSkeleton />
  }

  const page = pageQuery.data
  const path = { path: { status_page_id: page.id } }
  const canPublish = page.monitors.length > 0

  return (
    <div className="w-full min-w-0 space-y-6">
      <Link
        to="/status-pages"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="size-4" />
        Status pages
      </Link>

      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="truncate text-2xl font-bold tracking-tight">
              {page.title}
            </h2>
            <StatusPageStateBadge page={page} />
          </div>
          <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
            <span>Shows</span>
            <StatusPageProjects page={page} max={6} />
          </div>
          {!canPublish && (
            <p className="text-sm text-muted-foreground">
              Choose at least one component before publishing.
            </p>
          )}
        </div>
        <div className="flex gap-2">
          {page.is_published && page.public_url && (
            <Button variant="outline" asChild>
              <a href={page.public_url} target="_blank" rel="noreferrer">
                <ExternalLink className="size-4 sm:mr-2" />
                <span className="max-sm:sr-only">Open page</span>
              </a>
            </Button>
          )}
          {page.is_published ? (
            <Button
              variant="outline"
              disabled={unpublish.isPending}
              onClick={() => unpublish.mutate(path)}
            >
              <EyeOff className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">
                {unpublish.isPending ? 'Unpublishing...' : 'Unpublish'}
              </span>
            </Button>
          ) : (
            <Button
              disabled={!canPublish || publish.isPending}
              onClick={() => publish.mutate(path)}
            >
              <Eye className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">
                {publish.isPending ? 'Publishing...' : 'Publish'}
              </span>
            </Button>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="icon" aria-label="More actions">
                <MoreVertical className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setConfirmRotate(true)}>
                <RefreshCw className="mr-2 size-4" />
                Replace public link
              </DropdownMenuItem>
              <DropdownMenuItem
                className="text-destructive"
                onSelect={() => setConfirmDelete(true)}
              >
                <Trash2 className="mr-2 size-4" />
                Delete
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      <Tabs value={tab} onValueChange={setTab}>
        <TabsList>
          <TabsTrigger value="overview" icon={Home}>
            Overview
          </TabsTrigger>
          <TabsTrigger
            value="components"
            icon={Boxes}
            count={page.monitors.length}
          >
            Components
          </TabsTrigger>
          <TabsTrigger value="appearance" icon={Palette}>
            Appearance
          </TabsTrigger>
          <TabsTrigger value="incidents" icon={Siren}>
            Incidents
          </TabsTrigger>
          <TabsTrigger value="domains" icon={Globe}>
            Domains
          </TabsTrigger>
        </TabsList>
        <TabsContent value="overview" className="space-y-6 pt-4">
          <SetupChecklist
            page={page}
            onGoTo={setTab}
            onPublish={() => publish.mutate(path)}
            publishing={publish.isPending}
          />
          <PublicLinkSection page={page} />
          <StatusPagePreview page={page} />
        </TabsContent>
        <TabsContent value="components" className="pt-4">
          <StatusPageForm page={page} />
        </TabsContent>
        <TabsContent value="appearance" className="pt-4">
          <StatusPageAppearanceForm page={page} />
        </TabsContent>
        <TabsContent value="incidents" className="pt-4">
          {/* Keyed by page: switching pages must never show (and let you
              act on) the previous page's incidents or page number. */}
          <StatusPageIncidents key={page.id} page={page} />
        </TabsContent>
        <TabsContent value="domains" className="pt-4">
          <StatusPageDomains page={page} />
        </TabsContent>
      </Tabs>

      <AlertDialog open={confirmRotate} onOpenChange={setConfirmRotate}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Replace the public link?</AlertDialogTitle>
            <AlertDialogDescription>
              The current link stops working immediately. Anyone you shared it
              with will need the new one.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => rotate.mutate(path)}>
              {rotate.isPending ? 'Replacing...' : 'Replace link'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <ConfirmDeleteDialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        resourceType="status page"
        resourceName={page.title}
        consequences="The public link stops working immediately. Monitors and incidents are not affected."
        isPending={remove.isPending}
        onConfirm={() => remove.mutate(path)}
      />
    </div>
  )
}
