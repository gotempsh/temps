// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { GitCommitHorizontal } from 'lucide-react'
import {
  getErrorGroupSuspectCommitsOptions,
  getErrorSourceControlOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { SuspectCommitsView } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import { readFailureServerDetail } from '@/lib/read-failure'
import { RepositoryMissing } from './GroupIssues'
import { shortSha } from './source-control'

/** Where the error fails, and the commits that last touched that file. */
export function SuspectCommitList({ result }: { result: SuspectCommitsView }) {
  return (
    <div className="space-y-2">
      {result.path && (
        <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
          <code>
            {result.path}
            {result.lineno ? `:${result.lineno}` : ''}
          </code>
          {result.function ? ` in ${result.function}` : ''}
          {result.at ? ` at ${shortSha(result.at)}` : ' on the default branch'}
        </p>
      )}
      {result.commits.length === 0 ? (
        <p className="text-sm text-muted-foreground">{result.reason}</p>
      ) : (
        <ul className="space-y-2">
          {result.commits.map((commit) => (
            <li key={commit.sha} className="rounded-md border p-3 text-sm">
              <a
                href={commit.url}
                target="_blank"
                rel="noreferrer"
                className="font-medium [overflow-wrap:anywhere] hover:underline"
              >
                {commit.summary || shortSha(commit.sha)}
              </a>
              <p className="text-xs text-muted-foreground">
                <code>{shortSha(commit.sha)}</code>
                {commit.author_name ? ` · ${commit.author_name}` : ''}
                {commit.authored_at
                  ? ` · ${formatDistanceToNow(new Date(commit.authored_at), { addSuffix: true })}`
                  : ''}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/**
 * Suspect commits of an error group (ADR 111, D5): the newest commits that
 * touched the file of the latest event's innermost application frame, read
 * at the deployed commit.
 */
export function SuspectCommits({
  projectId,
  groupId,
}: {
  projectId: number
  groupId: number
}) {
  const status = useQuery({
    ...getErrorSourceControlOptions({ path: { project_id: projectId } }),
    staleTime: 60_000,
  })
  const configured = status.data?.configured === true
  const commits = useQuery({
    ...getErrorGroupSuspectCommitsOptions({
      path: { project_id: projectId, group_id: groupId },
    }),
    enabled: configured,
    staleTime: 5 * 60_000,
  })
  return (
    <section className="space-y-3" aria-label="Suspect commits">
      <h2 className="flex items-center gap-2 text-base font-semibold">
        <GitCommitHorizontal className="h-4 w-4" aria-hidden />
        Suspect commits
      </h2>
      {status.isPending ? (
        <Skeleton className="h-14 w-full" />
      ) : status.isError ? (
        <Alert variant="destructive">
          <AlertDescription>
            {readFailureServerDetail(status.error) ??
              'Whether this project has a repository could not be read. Retry.'}
          </AlertDescription>
        </Alert>
      ) : !configured ? (
        <RepositoryMissing
          status={status.data}
          example="With a GitHub or GitLab repository connected, this lists the latest commits that touched the file the error fails in, such as src/cart.ts:41."
        />
      ) : commits.isPending ? (
        <Skeleton className="h-14 w-full" />
      ) : commits.isError ? (
        <Alert variant="destructive">
          <AlertDescription>
            {readFailureServerDetail(commits.error) ??
              'The commits could not be read. Retry.'}
          </AlertDescription>
        </Alert>
      ) : commits.data ? (
        <SuspectCommitList result={commits.data} />
      ) : null}
    </section>
  )
}
