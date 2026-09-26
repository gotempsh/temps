// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, useState } from 'react'
import { GitBranch, Loader2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

export type WorkspaceGitBinding = {
  id: number
  projectId: number
  connectionId: number
  repositoryUrl: string
  remoteName: string
  status: 'configured'
}

export type WorkspaceGitRepository = {
  repositoryId: number
  connectionId: number
  accountName: string
  fullName: string
}

type Props = {
  projects: { id: number; name: string }[]
  bindings: WorkspaceGitBinding[]
  repositories: WorkspaceGitRepository[]
  loading?: boolean
  busy?: boolean
  error?: string | null
  onConnect: (selection: {
    projectId: number
    connectionId: number
    repositoryId: number
    remoteName: string
  }) => void
  onDisconnect: (bindingId: number) => void
}

/** Controlled view: the parent supplies only server-authorized repository choices. */
export function WorkspaceGitConnections({
  projects,
  bindings,
  repositories,
  loading = false,
  busy = false,
  error,
  onConnect,
  onDisconnect,
}: Props) {
  const id = useId()
  const [projectId, setProjectId] = useState('')
  const [repositoryKey, setRepositoryKey] = useState('')
  const [remoteName, setRemoteName] = useState('origin')
  const project = projects.find((item) => String(item.id) === projectId)
  const repository = repositories.find(
    (item) => `${item.connectionId}:${item.repositoryId}` === repositoryKey
  )
  const duplicate = bindings.some(
    (item) => item.projectId === project?.id && item.remoteName === remoteName
  )
  const validRemote = /^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$/.test(remoteName)

  return (
    <section
      className="space-y-4 rounded-xl border p-4"
      aria-labelledby={`${id}-title`}
    >
      <div>
        <h3 id={`${id}-title`} className="flex items-center gap-2 font-medium">
          <GitBranch className="size-4" /> Git connections
        </h3>
        <p className="mt-1 text-sm text-muted-foreground">
          Choose the account and repository for each project. Connections never
          fall back to another account.
        </p>
      </div>
      {loading ? (
        <p role="status" className="flex items-center gap-2 text-sm">
          <Loader2 className="size-4 animate-spin" /> Loading Git connections…
        </p>
      ) : (
        <>
          {bindings.length > 0 && (
            <ul className="divide-y rounded-lg border">
              {bindings.map((binding) => (
                <li key={binding.id} className="space-y-2 p-3">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-sm font-medium">
                      {projects.find((item) => item.id === binding.projectId)
                        ?.name ?? `Project ${binding.projectId}`}{' '}
                      / <code>{binding.remoteName}</code>
                    </span>
                    <Button
                      type="button"
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      onClick={() => onDisconnect(binding.id)}
                      aria-label={`Disconnect ${binding.remoteName} from project ${binding.projectId}`}
                    >
                      Disconnect
                    </Button>
                  </div>
                  <p className="break-all font-mono text-xs">
                    {binding.repositoryUrl}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    Configured · account connection #{binding.connectionId}.
                    This records access; it does not publish files.
                  </p>
                </li>
              ))}
            </ul>
          )}
          {repositories.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No eligible repositories. Add a Git provider connection or
              personal access token in Git settings, then synchronize its
              repositories.
            </p>
          ) : (
            <form
              className="space-y-3"
              onSubmit={(event) => {
                event.preventDefault()
                if (
                  !busy &&
                  project &&
                  repository &&
                  validRemote &&
                  !duplicate
                ) {
                  onConnect({
                    projectId: project.id,
                    connectionId: repository.connectionId,
                    repositoryId: repository.repositoryId,
                    remoteName,
                  })
                }
              }}
            >
              <div className="space-y-1">
                <Label htmlFor={`${id}-project`}>Project</Label>
                <select
                  id={`${id}-project`}
                  className="h-9 w-full rounded-md border bg-background px-2 text-sm"
                  value={projectId}
                  disabled={busy}
                  onChange={(event) => setProjectId(event.target.value)}
                >
                  <option value="">Choose a project</option>
                  {projects.map((item) => (
                    <option key={item.id} value={item.id}>
                      {item.name}
                    </option>
                  ))}
                </select>
              </div>
              <div className="space-y-1">
                <Label htmlFor={`${id}-repository`}>Account / repository</Label>
                <select
                  id={`${id}-repository`}
                  className="h-9 w-full rounded-md border bg-background px-2 text-sm"
                  value={repositoryKey}
                  disabled={busy}
                  onChange={(event) => setRepositoryKey(event.target.value)}
                >
                  <option value="">Choose a repository</option>
                  {repositories.map((item) => (
                    <option
                      key={`${item.connectionId}:${item.repositoryId}`}
                      value={`${item.connectionId}:${item.repositoryId}`}
                    >
                      {item.accountName} · {item.fullName} · connection #
                      {item.connectionId}
                    </option>
                  ))}
                </select>
              </div>
              <div className="space-y-1">
                <Label htmlFor={`${id}-remote`}>Remote name</Label>
                <Input
                  id={`${id}-remote`}
                  value={remoteName}
                  disabled={busy}
                  maxLength={64}
                  onChange={(event) => setRemoteName(event.target.value)}
                />
              </div>
              {duplicate && (
                <p role="alert" className="text-sm text-destructive">
                  This project already has that remote. Disconnect it before
                  choosing another account.
                </p>
              )}
              <Button
                type="submit"
                disabled={
                  busy || !project || !repository || !validRemote || duplicate
                }
              >
                {busy ? 'Saving…' : 'Connect repository'}
              </Button>
            </form>
          )}
        </>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <a
        className="inline-block text-sm underline underline-offset-4"
        href="/git-providers"
      >
        Manage Git providers and access tokens
      </a>
    </section>
  )
}
