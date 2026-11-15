// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getProjectsOptions,
  linkServiceToProjectMutation,
  listServiceProjectsOptions,
  listServiceProjectsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { toastRedeployAfterLinkChange } from '@/lib/service-link-redeploy'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link2, Loader2 } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { invalidateChatResources } from './chat-resources'
import { linkCandidates } from './link-service'

/**
 * "Link" for a database or service the chat made: lets a project use it, the
 * same link the service's own page makes. The projects this chat made are
 * offered first, since that is almost always the one meant.
 */
export function LinkServiceButton({
  serviceId,
  serviceName,
  chatProjectIds,
}: {
  serviceId: number
  serviceName: string
  chatProjectIds: readonly number[]
}) {
  const [open, setOpen] = useState(false)
  const queryClient = useQueryClient()
  const linkedOptions = { path: { id: serviceId } }
  const linked = useQuery(listServiceProjectsOptions(linkedOptions))
  const projects = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    enabled: open,
  })
  const link = useMutation({
    ...linkServiceToProjectMutation(),
    meta: { errorTitle: `Failed to link ${serviceName}` },
    onSuccess: (_data, variables) => {
      void queryClient.invalidateQueries({
        queryKey: listServiceProjectsQueryKey(linkedOptions),
      })
      void invalidateChatResources(queryClient)
      setOpen(false)
      const project = projects.data?.projects?.find(
        (candidate) => candidate.id === variables.body.project_id
      )
      toast.success(
        project
          ? `${serviceName} linked to ${project.name}`
          : `${serviceName} linked`
      )
      if (project) {
        void toastRedeployAfterLinkChange(
          project,
          { kind: 'linked', serviceName },
          toast
        )
      }
    },
  })

  const linkedProjects = linked.data ?? []
  const { chat, other } = linkCandidates(
    projects.data?.projects ?? [],
    linkedProjects.map((entry) => entry.project.id),
    chatProjectIds
  )
  const usedBy = linkedProjects.map((entry) => entry.project.slug).join(', ')

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-8 shrink-0 gap-1.5"
          disabled={link.isPending}
          title={
            usedBy
              ? `${serviceName} is used by ${usedBy}. Link it to another project.`
              : `Let a project use ${serviceName}: its deployments get the connection variables.`
          }
          aria-label={`Link ${serviceName} to a project`}
        >
          {link.isPending ? (
            <Loader2 className="size-4 animate-spin" aria-hidden />
          ) : (
            <Link2 className="size-4" aria-hidden />
          )}
          <span className="max-sm:sr-only">Link</span>
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-80 p-0" align="end">
        <div className="border-b p-3">
          <p className="text-xs font-medium text-muted-foreground">
            Projects using {serviceName}
          </p>
          {linked.isPending ? (
            <p className="mt-2 text-xs text-muted-foreground">Loading…</p>
          ) : linkedProjects.length > 0 ? (
            <ul className="mt-2 space-y-1">
              {linkedProjects.map((entry) => (
                <li
                  key={entry.id}
                  className="flex items-center justify-between gap-2 text-sm"
                >
                  <span className="truncate">{entry.project.slug}</span>
                  <Link
                    to={`/projects/${entry.project.slug}`}
                    className="text-xs text-muted-foreground hover:text-foreground"
                  >
                    View →
                  </Link>
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-2 text-xs text-muted-foreground">
              None yet. A linked project&rsquo;s deployments get the connection
              variables, e.g. <code>DATABASE_URL</code>.
            </p>
          )}
        </div>
        <Command>
          <CommandInput placeholder="Link a project…" />
          <CommandList>
            <CommandEmpty>
              {projects.isPending ? 'Loading projects…' : 'No projects found.'}
            </CommandEmpty>
            {chat.length > 0 && (
              <CommandGroup heading="Made in this chat">
                {chat.map((project) => (
                  <CommandItem
                    key={project.id}
                    value={`${project.name} ${project.slug}`}
                    onSelect={() =>
                      link.mutate({
                        path: { id: serviceId },
                        body: { project_id: project.id },
                      })
                    }
                  >
                    {project.name}
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {other.length > 0 && (
              <CommandGroup heading="Other projects">
                {other.map((project) => (
                  <CommandItem
                    key={project.id}
                    value={`${project.name} ${project.slug}`}
                    onSelect={() =>
                      link.mutate({
                        path: { id: serviceId },
                        body: { project_id: project.id },
                      })
                    }
                  >
                    {project.name}
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
