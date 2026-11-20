// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleDot, ExternalLink, Link2, RefreshCw, Unlink } from 'lucide-react'
import { toast } from 'sonner'
import {
  createErrorGroupIssueMutation,
  getErrorSourceControlOptions,
  linkErrorGroupIssueMutation,
  listErrorGroupActivityQueryKey,
  listErrorGroupIssuesOptions,
  listErrorGroupIssuesQueryKey,
  refreshErrorGroupIssuesMutation,
  unlinkErrorGroupIssueMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  IssueLinkView,
  SourceControlStatusView,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { readFailureServerDetail } from '@/lib/read-failure'
import { queryKeyPrefix } from '@/lib/refresh'
import {
  GIT_PROVIDERS_PATH,
  issueLabel,
  linkIssueSchema,
  MAX_LINKS_PER_GROUP,
  openIssueSchema,
  providerName,
  type LinkIssueFormData,
  type OpenIssueFormData,
} from './source-control'

/** What opening an issue needs, when the repository cannot be reached. */
export function RepositoryMissing({
  status,
  example,
}: {
  status: SourceControlStatusView
  example: string
}) {
  return (
    <Alert>
      <AlertDescription className="space-y-2">
        <p>{status.reason}</p>
        <p className="text-muted-foreground">{example}</p>
        <Link
          className="font-medium underline"
          to={status.setup_path ?? GIT_PROVIDERS_PATH}
        >
          Connect a repository
        </Link>
      </AlertDescription>
    </Alert>
  )
}

function StateBadge({ link }: { link: IssueLinkView }) {
  if (!link.state) return null
  return (
    <Badge variant={link.state === 'open' ? 'default' : 'secondary'}>
      {link.state === 'open' ? 'Open' : 'Closed'}
    </Badge>
  )
}

/** The issues linked to a group, as a list. */
export function IssueList({
  links,
  onUnlink,
}: {
  links: IssueLinkView[]
  onUnlink: (link: IssueLinkView) => void
}) {
  return (
    <ul className="space-y-2">
      {links.map((link) => (
        <li
          key={link.id}
          className="flex items-start gap-2 rounded-md border p-3 text-sm"
        >
          <CircleDot
            className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
            aria-hidden
          />
          <div className="min-w-0 flex-1 space-y-1">
            <a
              href={link.url}
              target="_blank"
              rel="noreferrer"
              className="font-medium [overflow-wrap:anywhere] hover:underline"
            >
              {link.title}
              <ExternalLink className="ml-1 inline h-3 w-3" aria-hidden />
            </a>
            <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
              <span className="[overflow-wrap:anywhere]">
                {issueLabel(link)}
              </span>
              <StateBadge link={link} />
            </div>
          </div>
          <Button
            variant="ghost"
            size="sm"
            aria-label={`Unlink ${link.title}`}
            onClick={() => onUnlink(link)}
          >
            <Unlink className="h-4 w-4" />
          </Button>
        </li>
      ))}
    </ul>
  )
}

function LinkIssueDialog({
  open,
  onOpenChange,
  pending,
  onSubmit,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  pending: boolean
  onSubmit: (data: LinkIssueFormData) => void
}) {
  const form = useForm<LinkIssueFormData>({
    resolver: zodResolver(linkIssueSchema),
    defaultValues: { url: '', title: '' },
  })
  const { errors } = form.formState
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) form.reset()
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form noValidate onSubmit={form.handleSubmit(onSubmit)}>
          <DialogHeader>
            <DialogTitle>Link an issue</DialogTitle>
            <DialogDescription>
              Paste the issue from any tracker. An issue of this project&apos;s
              repository shows whether it is open.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-1.5">
              <Label htmlFor="issue-url">Issue URL</Label>
              <Input
                id="issue-url"
                type="url"
                inputMode="url"
                placeholder="https://tracker.example.com/browse/SHOP-12"
                aria-invalid={errors.url ? true : undefined}
                aria-describedby={errors.url ? 'issue-url-error' : undefined}
                {...form.register('url')}
              />
              {errors.url && (
                <p id="issue-url-error" className="text-xs text-destructive">
                  {errors.url.message}
                </p>
              )}
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="issue-title">Title (optional)</Label>
              <Input
                id="issue-title"
                aria-invalid={errors.title ? true : undefined}
                {...form.register('title')}
              />
              {errors.title && (
                <p className="text-xs text-destructive">
                  {errors.title.message}
                </p>
              )}
            </div>
          </div>
          <DialogFooter>
            <Button type="submit" disabled={pending}>
              {pending ? 'Linking…' : 'Link issue'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function OpenIssueDialog({
  open,
  onOpenChange,
  status,
  defaultTitle,
  pending,
  onSubmit,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  status: SourceControlStatusView
  defaultTitle: string
  pending: boolean
  onSubmit: (data: OpenIssueFormData) => void
}) {
  const form = useForm<OpenIssueFormData>({
    resolver: zodResolver(openIssueSchema),
    defaultValues: { title: defaultTitle },
  })
  const { errors } = form.formState
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) form.reset({ title: defaultTitle })
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form noValidate onSubmit={form.handleSubmit(onSubmit)}>
          <DialogHeader>
            <DialogTitle>Open an issue in {status.repository}</DialogTitle>
            <DialogDescription>
              The issue gets the error, how often it happened, its stack and a
              link back here. Request data, users and tags are not included.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-1.5 py-4">
            <Label htmlFor="new-issue-title">Title</Label>
            <Input
              id="new-issue-title"
              aria-invalid={errors.title ? true : undefined}
              {...form.register('title')}
            />
            {errors.title && (
              <p className="text-xs text-destructive">{errors.title.message}</p>
            )}
          </div>
          <DialogFooter>
            <Button type="submit" disabled={pending}>
              {pending
                ? 'Opening…'
                : `Open in ${providerName(status.provider)}`}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

/**
 * Issues of an error group (ADR 111, D5): open one in the project's GitHub
 * or GitLab repository, link one from any tracker, see whether each is
 * open, and unlink one. Unlinking leaves the issue as it is.
 */
export function GroupIssues({
  projectId,
  groupId,
  groupTitle,
}: {
  projectId: number
  groupId: number
  groupTitle: string
}) {
  const queryClient = useQueryClient()
  const path = { project_id: projectId, group_id: groupId }
  const status = useQuery({
    ...getErrorSourceControlOptions({ path: { project_id: projectId } }),
    staleTime: 60_000,
  })
  const list = useQuery(listErrorGroupIssuesOptions({ path }))
  const [linking, setLinking] = useState(false)
  const [opening, setOpening] = useState(false)
  const [unlinking, setUnlinking] = useState<IssueLinkView | null>(null)

  const changed = () => {
    void queryClient.invalidateQueries({
      queryKey: queryKeyPrefix(listErrorGroupIssuesQueryKey, { path }),
    })
    void queryClient.invalidateQueries({
      queryKey: queryKeyPrefix(listErrorGroupActivityQueryKey, { path }),
    })
  }
  const create = useMutation({
    ...createErrorGroupIssueMutation(),
    meta: { errorTitle: 'Failed to open the issue' },
    onSuccess: (link) => {
      toast.success(`Opened ${issueLabel(link)}`)
      setOpening(false)
      changed()
    },
  })
  const link = useMutation({
    ...linkErrorGroupIssueMutation(),
    meta: { errorTitle: 'Failed to link the issue' },
    onSuccess: () => {
      toast.success('Issue linked')
      setLinking(false)
      changed()
    },
  })
  const refresh = useMutation({
    ...refreshErrorGroupIssuesMutation(),
    meta: { errorTitle: 'Failed to read the issues' },
    onSuccess: (result) => {
      if (result.failed_refreshes > 0) {
        toast.warning(
          `${result.failed_refreshes} issue${result.failed_refreshes === 1 ? '' : 's'} could not be read; they keep their last state`
        )
      }
      queryClient.setQueryData(listErrorGroupIssuesQueryKey({ path }), result)
    },
  })
  const unlink = useMutation({
    ...unlinkErrorGroupIssueMutation(),
    meta: { errorTitle: 'Failed to unlink the issue' },
    onSuccess: () => {
      toast.success('Issue unlinked')
      changed()
    },
    onSettled: () => setUnlinking(null),
  })

  const links = list.data?.data ?? []
  const full = links.length >= MAX_LINKS_PER_GROUP
  const tracked = links.some((l) => l.provider !== 'link')
  const repository = status.data

  return (
    <section className="space-y-3" aria-label="Issues">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="flex items-center gap-2 text-base font-semibold">
          <CircleDot className="h-4 w-4" aria-hidden />
          Issues
        </h2>
        <div className="flex flex-wrap gap-2">
          {tracked && (
            <Button
              variant="ghost"
              size="sm"
              disabled={refresh.isPending}
              onClick={() => refresh.mutate({ path })}
            >
              <RefreshCw className="h-4 w-4" />
              <span className="max-sm:sr-only">Refresh state</span>
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            disabled={full}
            onClick={() => setLinking(true)}
          >
            <Link2 className="h-4 w-4" />
            Link issue
          </Button>
          {repository?.configured && (
            <Button size="sm" disabled={full} onClick={() => setOpening(true)}>
              Open issue
            </Button>
          )}
        </div>
      </div>
      {status.isError && (
        <p className="text-xs text-muted-foreground">
          {readFailureServerDetail(status.error) ??
            'Whether this project has a repository could not be read, so Open issue is unavailable. Links still work.'}
        </p>
      )}
      {repository && !repository.configured && (
        <RepositoryMissing
          status={repository}
          example="With a GitHub or GitLab repository connected, Open issue files this error there with its stack and a link back, and shows whether the issue is still open."
        />
      )}
      {list.isPending ? (
        <Skeleton className="h-14 w-full" />
      ) : list.isError ? (
        <Alert variant="destructive">
          <AlertDescription>
            {readFailureServerDetail(list.error) ??
              'The issues could not be loaded. Retry.'}
          </AlertDescription>
        </Alert>
      ) : links.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No issue tracks this error yet.
        </p>
      ) : (
        <IssueList links={links} onUnlink={setUnlinking} />
      )}
      {full && (
        <p className="text-xs text-muted-foreground">
          A group has at most {MAX_LINKS_PER_GROUP} issues. Unlink one to add
          another.
        </p>
      )}
      <LinkIssueDialog
        open={linking}
        onOpenChange={setLinking}
        pending={link.isPending}
        onSubmit={(data) =>
          link.mutate({
            path,
            body: { url: data.url, title: data.title || null },
          })
        }
      />
      {repository?.configured && (
        <OpenIssueDialog
          open={opening}
          onOpenChange={setOpening}
          status={repository}
          defaultTitle={groupTitle.slice(0, 255)}
          pending={create.isPending}
          onSubmit={(data) =>
            create.mutate({ path, body: { title: data.title || null } })
          }
        />
      )}
      <ConfirmDeleteDialog
        open={unlinking !== null}
        onOpenChange={(open) => {
          if (!open) setUnlinking(null)
        }}
        resourceType="issue link"
        resourceName={unlinking ? issueLabel(unlinking) : ''}
        consequences="The issue stays as it is in its tracker; only the link from this error group is removed."
        isPending={unlink.isPending}
        onConfirm={() => {
          if (unlinking) {
            unlink.mutate({
              path: { ...path, link_id: unlinking.id },
            })
          }
        }}
      />
    </section>
  )
}
