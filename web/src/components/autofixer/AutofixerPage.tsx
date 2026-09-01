// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { listErrorGroupsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useQuery } from '@tanstack/react-query'
import { Sparkles, Wand2 } from 'lucide-react'
import { useNavigate } from 'react-router'
import { useAutofixReadiness } from '@/hooks/useAutofixReadiness'
import { useAutofixOnboarding } from './AutofixOnboardingContext'

interface AutofixerPageProps {
  project: ProjectResponse
}

function formatTimeAgo(dateStr: string): string {
  const diffMs = Date.now() - new Date(dateStr).getTime()
  const diffMins = Math.floor(diffMs / 60000)
  if (diffMins < 60) return `${diffMins}m ago`
  const diffHours = Math.floor(diffMins / 60)
  if (diffHours < 24) return `${diffHours}h ago`
  return `${Math.floor(diffHours / 24)}d ago`
}

export function AutofixerPage({ project }: AutofixerPageProps) {
  const navigate = useNavigate()
  const { openOnboarding } = useAutofixOnboarding()
  // A public URL-only repo has `hasRepo` but not `connected`: autofix can read
  // it yet can't push a branch or open a PR, so readiness reports it as unmet
  // with wording that names the real problem.
  const git = {
    connected: !!project.git_provider_connection_id,
    hasRepo: !!project.repo_owner && !!project.repo_name,
    label:
      project.repo_owner && project.repo_name
        ? `${project.repo_owner}/${project.repo_name}`
        : undefined,
  }
  const readiness = useAutofixReadiness({
    projectId: project.id,
    projectSlug: project.slug,
    projectGit: git,
  })

  // Fetch unresolved error groups to show fixable errors
  const { data: errorGroups, isLoading } = useQuery({
    ...listErrorGroupsOptions({
      path: { project_id: project.id },
      query: { status: 'unresolved', page_size: 20, page: 1 },
    }),
  })

  const groups =
    (errorGroups as any)?.data ||
    (errorGroups as any)?.items ||
    errorGroups ||
    []

  if (isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Wand2 className="h-5 w-5 text-muted-foreground" />
          <h1 className="text-xl font-semibold">Autofixer</h1>
        </div>
      </div>

      <p className="text-sm text-muted-foreground">
        Select an error to analyze and fix with AI. Claude will read your
        codebase, identify the root cause, and generate a fix.
      </p>

      {!readiness.isLoading && !readiness.isReady && (
        <Card>
          <CardContent className="flex flex-col items-start gap-3 p-6 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <p className="text-sm font-medium">Autofix isn't set up yet</p>
              <p className="text-xs text-muted-foreground">
                {readiness.firstIncomplete?.description ??
                  'Finish setup to run AI fixes on your errors.'}
              </p>
            </div>
            <Button
              size="sm"
              className="shrink-0"
              onClick={() =>
                openOnboarding({
                  projectId: project.id,
                  projectSlug: project.slug,
                  projectGit: git,
                })
              }
            >
              <Wand2 className="size-4" />
              Set up autofix
            </Button>
          </CardContent>
        </Card>
      )}

      {groups.length === 0 && (
        <Card>
          <CardContent className="p-12 flex flex-col items-center justify-center text-center">
            <Sparkles className="h-12 w-12 text-muted-foreground mb-4" />
            <h2 className="text-lg font-semibold mb-2">No unresolved errors</h2>
            <p className="text-sm text-muted-foreground">
              When errors occur, they'll appear here and you can fix them with
              AI.
            </p>
          </CardContent>
        </Card>
      )}

      {/* The error list is shown regardless of setup state — the card above
          explains what's missing. Hiding the errors as well left the page
          looking empty and gave no hint that autofix applies to them. */}
      {groups.length > 0 && (
        <Card>
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Error</TableHead>
                  <TableHead className="hidden md:table-cell">Type</TableHead>
                  <TableHead className="hidden md:table-cell">Count</TableHead>
                  <TableHead className="hidden md:table-cell">
                    Last seen
                  </TableHead>
                  <TableHead className="w-[180px]">Autofix</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {groups.map((group: any) => (
                  <TableRow key={group.id}>
                    <TableCell className="max-w-[400px]">
                      <p className="font-medium truncate">{group.title}</p>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <span className="text-xs bg-muted px-2 py-0.5 rounded">
                        {group.error_type}
                      </span>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      {group.total_count}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-muted-foreground text-sm">
                      {group.last_seen ? formatTimeAgo(group.last_seen) : '-'}
                    </TableCell>
                    <TableCell>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() =>
                          navigate(
                            `/projects/${project.slug}/errors/${group.id}/autofix`
                          )
                        }
                      >
                        <Wand2 className="h-3 w-3 mr-1" />
                        Fix
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </Card>
      )}
    </div>
  )
}
