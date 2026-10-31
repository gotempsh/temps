// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'

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
import { Switch } from '@/components/ui/switch'
import { ChoiceList, type Choice } from '@/components/ui/choice-list'
import {
  ArrowRight,
  Container,
  FileArchive,
  GitBranch,
  Loader2,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router'
import { settingsSectionHref } from '@/lib/project-settings-sections'
import { toast } from 'sonner'

type SourceType =
  'git' | 'docker_image' | 'static_files' | 'uploaded_source' | 'manual'

/** How each source type reads in the "keeps deploying from X" sentence. */
const SOURCE_LABELS: Record<SourceType, string> = {
  git: 'its Git repository',
  docker_image: 'a Docker image',
  static_files: 'an uploaded static bundle',
  uploaded_source: 'an uploaded source archive',
  manual: 'its configured source',
}

/**
 * The source types this card can switch a project *to*.
 *
 * `uploaded_source` is deliberately absent: it is what `drop` sets when it
 * creates a project, and a project should gain the ability to take archives
 * through the alternate-sources opt-in below — which keeps its existing source
 * intact — rather than by being converted into an upload-only project.
 */
type PickableSourceType = 'git' | 'docker_image' | 'static_files'

function isPickable(value: SourceType): value is PickableSourceType {
  return value === 'git' || value === 'docker_image' || value === 'static_files'
}

const SOURCE_TYPES: Choice<PickableSourceType>[] = [
  {
    value: 'git',
    label: 'Git repository',
    description:
      'Build and deploy from a connected Git repository on every push.',
    icon: GitBranch,
  },
  {
    value: 'docker_image',
    label: 'Docker image',
    description:
      'Deploy a prebuilt image pulled from a registry (no build step).',
    icon: Container,
  },
  {
    value: 'static_files',
    label: 'Static files',
    description: 'Deploy an uploaded static bundle (.zip / .tar.gz).',
    icon: FileArchive,
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
  const [selected, setSelected] = useState<PickableSourceType>(
    isPickable(current) ? current : 'docker_image'
  )
  const [switching, setSwitching] = useState(false)
  const [savingAlternates, setSavingAlternates] = useState(false)
  const allowsAlternates = project.allow_alternate_sources === true
  // An uploaded-source project already accepts archives by definition, so the
  // opt-in would be a no-op control — say so rather than offering a dead toggle.
  const alternatesAreImplicit = current === 'uploaded_source'

  const selectedMeta = SOURCE_TYPES.find((t) => t.value === selected)
  const isCurrent = selected === current
  // A repository can already be configured even when the source type isn't
  // git — in that case "switch to Git" is a direct flip, not a fresh setup.
  const hasGitConfig =
    !!project.repo_owner &&
    !!project.repo_name &&
    project.repo_owner !== 'unknown' &&
    project.repo_name !== 'unknown'

  const setAlternates = async (allow: boolean) => {
    setSavingAlternates(true)
    try {
      const res = await fetch(`/api/projects/${project.id}/alternate-sources`, {
        method: 'PATCH',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ allow_alternate_sources: allow }),
      })
      if (!res.ok) {
        const d = (await res.json().catch(() => null)) as {
          detail?: string
        } | null
        throw new Error(d?.detail || 'Failed to update alternate sources')
      }
      toast.success(
        allow
          ? 'This service can now also be deployed from an uploaded folder'
          : 'This service now only deploys from its configured source'
      )
      refetch()
    } catch (e) {
      toast.error(
        (e as { message?: string })?.message ||
          'Failed to update alternate sources'
      )
    } finally {
      setSavingAlternates(false)
    }
  }

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
    <>
      <Card id="setting-deployment-source" className="scroll-mt-4">
        <CardHeader>
          <CardTitle>Deployment source</CardTitle>
          <CardDescription>
            How this service is built and deployed.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <ChoiceList
            label="Deployment source"
            value={selected}
            onValueChange={setSelected}
            choices={SOURCE_TYPES}
            disabled={switching}
          />
        </CardContent>
        <CardFooter>
          <p>
            {isPickable(current)
              ? isCurrent
                ? null
                : `Deploys from ${SOURCE_LABELS[current]} until you save.`
              : `Deploys from ${SOURCE_LABELS[current]} today. Pick a source to switch to.`}
          </p>
          {selected === 'git' && !isCurrent && !hasGitConfig ? (
            // No repository configured yet — hand off to the Repository
            // section, whose save configures the repo and flips it to Git.
            <Button
              size="sm"
              onClick={() =>
                navigate(
                  settingsSectionHref(project.slug, 'delivery', 'repository')
                )
              }
            >
              Set up repository
              <ArrowRight className="ml-2 h-4 w-4" />
            </Button>
          ) : (
            <Button
              size="sm"
              onClick={switchTo}
              disabled={isCurrent || switching}
            >
              {switching && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              Save
            </Button>
          )}
        </CardFooter>
      </Card>

      <Card id="setting-alternate-sources" className="scroll-mt-4">
        <CardHeader className="flex-row items-start justify-between gap-4">
          <div className="min-w-0 space-y-1">
            <CardTitle>Other sources</CardTitle>
            <CardDescription>
              Keep deploying from {SOURCE_LABELS[current]} by default, and also
              accept a local folder pushed straight to this service.
            </CardDescription>
          </div>
          <Switch
            checked={alternatesAreImplicit || allowsAlternates}
            disabled={savingAlternates || alternatesAreImplicit}
            onCheckedChange={setAlternates}
            aria-label="Allow deploying this service from other sources"
          />
        </CardHeader>
        <CardContent>
          <HighlightedCode
            language="bash"
            code={`bunx @temps-sdk/cli drop ./ --project ${project.slug}`}
            className="block break-all rounded bg-muted px-3 py-2 text-xs"
          />
        </CardContent>
        <CardFooter>
          <p>
            {alternatesAreImplicit
              ? 'This service already deploys from uploaded source, so it always accepts one.'
              : 'Uploading a folder re-detects the build directory and preset, which is why it is off by default. Docker images and static bundles are always accepted.'}
          </p>
        </CardFooter>
      </Card>
    </>
  )
}
