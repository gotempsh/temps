import { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { cn } from '@/lib/utils'
import {
  ArrowRight,
  Check,
  Container,
  FileArchive,
  GitBranch,
  Loader2,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'

type SourceType = 'git' | 'docker_image' | 'static_files' | 'manual'

const SOURCE_TYPES: {
  value: Exclude<SourceType, 'manual'>
  label: string
  desc: string
  Icon: typeof GitBranch
}[] = [
  {
    value: 'git',
    label: 'Git repository',
    desc: 'Build and deploy from a connected Git repository on every push.',
    Icon: GitBranch,
  },
  {
    value: 'docker_image',
    label: 'Docker image',
    desc: 'Deploy a prebuilt image pulled from a registry (no build step).',
    Icon: Container,
  },
  {
    value: 'static_files',
    label: 'Static files',
    desc: 'Deploy an uploaded static bundle (.zip / .tar.gz).',
    Icon: FileArchive,
  },
]

/**
 * Choose how a project is built and deployed.
 *
 * - Switching to **Docker image** / **Static files** is a one-click change
 *   (`PATCH /projects/{id}/source`).
 * - Switching to **Git** needs a repository + provider connection, so it hands
 *   off to the Git settings page (which has the full repo/branch/preset picker
 *   and flips `source_type` to `git` on save).
 */
export function DeploymentSourceCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const navigate = useNavigate()
  const current = (project.source_type ?? 'git') as SourceType
  const [selected, setSelected] = useState<Exclude<SourceType, 'manual'>>(
    current === 'manual' ? 'docker_image' : current
  )
  const [switching, setSwitching] = useState(false)

  const selectedMeta = SOURCE_TYPES.find((t) => t.value === selected)
  const isCurrent = selected === current
  // A repository can already be configured even when the source type isn't
  // git — in that case "switch to Git" is a direct flip, not a fresh setup.
  const hasGitConfig =
    !!project.repo_owner &&
    !!project.repo_name &&
    project.repo_owner !== 'unknown' &&
    project.repo_name !== 'unknown'

  const switchTo = async () => {
    setSwitching(true)
    try {
      const res = await fetch(`/api/projects/${project.id}/source`, {
        method: 'PATCH',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ source_type: selected }),
      })
      if (!res.ok) {
        const d = (await res.json().catch(() => null)) as {
          detail?: string
        } | null
        throw new Error(d?.detail || 'Failed to change deployment source')
      }
      toast.success(`Deployment source changed to ${selectedMeta?.label}`)
      refetch()
    } catch (e) {
      toast.error(
        (e as { message?: string })?.message || 'Failed to change source'
      )
    } finally {
      setSwitching(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Deployment source</CardTitle>
        <CardDescription>How this project is built and deployed.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {SOURCE_TYPES.map((t) => (
          <button
            key={t.value}
            type="button"
            onClick={() => setSelected(t.value)}
            className={cn(
              'flex w-full items-start gap-3 rounded-lg border p-3 text-left transition-colors',
              selected === t.value
                ? 'border-primary ring-1 ring-primary'
                : 'border-border hover:bg-accent'
            )}
          >
            <t.Icon className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 text-sm font-medium">
                {t.label}
                {current === t.value && (
                  <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-normal text-muted-foreground">
                    current
                  </span>
                )}
              </div>
              <div className="text-sm text-muted-foreground">{t.desc}</div>
            </div>
            {selected === t.value && (
              <Check className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
            )}
          </button>
        ))}
      </CardContent>
      <CardFooter>
        {isCurrent ? (
          <p className="text-sm text-muted-foreground">
            This is the current deployment source.
          </p>
        ) : selected === 'git' && !hasGitConfig ? (
          // No repository configured yet — hand off to the Git settings page,
          // whose save configures the repo and flips the project to Git.
          <Button onClick={() => navigate('../git')}>
            Set up Git repository
            <ArrowRight className="ml-2 h-4 w-4" />
          </Button>
        ) : (
          <Button onClick={switchTo} disabled={switching}>
            {switching && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            Switch to {selectedMeta?.label}
          </Button>
        )}
      </CardFooter>
    </Card>
  )
}
