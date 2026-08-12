import {
  deployFromStatic,
  deployFromUploadedSource,
  getEnvironments,
  inspectDropArchive,
  uploadStaticBundle,
  updateProjectSettings,
  type DropInspectionResponse,
  type EnvironmentResponse,
  type ProjectResponse,
} from '@/api/client'
import { DetectedPresetCard } from '@/components/drop/DetectedPresetCard'
import { DetectedPresetGrid } from '@/components/drop/DetectedPresetGrid'
import { DropZone } from '@/components/drop/DropZone'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  htmlRootCandidates,
  isDropArchive,
  type DropFile,
} from '@/lib/drop-archive'
import { dropErrorMessage } from '@/lib/drop-files'
import { prepareAndInspectDrop } from '@/lib/drop-preset-detection'
import { cn } from '@/lib/utils'
import { Loader2, UploadCloud, X } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useNavigate } from 'react-router'

type Stage =
  'idle' | 'packing' | 'detecting' | 'saving' | 'uploading' | 'deploying'

function stageLabel(
  stage: Stage,
  detectedLabel?: string,
  hasFiles = false
): string {
  switch (stage) {
    case 'packing':
      return 'Packing files locally'
    case 'detecting':
      return 'Detecting preset'
    case 'saving':
      return 'Updating build configuration'
    case 'uploading':
      return 'Uploading source'
    case 'deploying':
      return 'Starting deployment'
    default:
      if (detectedLabel) return `Deploy ${detectedLabel}`
      return hasFiles ? 'Retry preset detection' : 'Select project files'
  }
}

/**
 * Deploy a new source archive into an **existing** project — the same gesture
 * as `/drop`, but targeting a project that already exists instead of creating
 * one.
 *
 * This replaced a plain `<input type="file">` dialog that accepted only a
 * `.zip` and did no detection. That was a worse experience than the front-door
 * `/drop` page for the exact users most likely to repeat the action, so both
 * paths now share `DropZone` and the same detect-then-deploy flow: folder,
 * single HTML file, or ZIP.
 *
 * **What this page may change about the project:** the build `directory` and
 * `preset` only, and only when the user picks a candidate that differs from
 * what the project already has. Port, resource limits, environment variables,
 * domains and every other setting are deliberately left alone — re-uploading
 * source is not an invitation to silently re-provision the service.
 */
export function ProjectDrop({ project }: { project: ProjectResponse }) {
  const navigate = useNavigate()
  const [files, setFiles] = useState<DropFile[]>([])
  const [rootPage, setRootPage] = useState('')
  const [stage, setStage] = useState<Stage>('idle')
  const [error, setError] = useState<string | null>(null)
  const [preparedArchive, setPreparedArchive] = useState<File | null>(null)
  const [inspection, setInspection] = useState<DropInspectionResponse | null>(
    null
  )
  const [candidateIndex, setCandidateIndex] = useState('0')
  const [environments, setEnvironments] = useState<EnvironmentResponse[]>([])
  const [environmentId, setEnvironmentId] = useState('')
  const detectionRunRef = useRef(0)
  const detectionAbortRef = useRef<AbortController | null>(null)

  usePageTitle(`Upload source · ${project.name}`)

  const isBusy = stage !== 'idle'
  const htmlPages = useMemo(() => htmlRootCandidates(files), [files])
  const hasRootIndex = htmlPages.some((p) => p.toLowerCase() === 'index.html')
  const isArchive = files.length === 1 && isDropArchive(files[0].file.name)
  const candidate = inspection?.candidates[Number(candidateIndex)]

  // Environments are needed before the first deploy; production is the sane
  // default so the common case is zero clicks.
  useEffect(() => {
    let cancelled = false
    getEnvironments({ path: { project_id: project.id } })
      .then((result) => {
        if (cancelled) return
        const list = result.data || []
        setEnvironments(list)
        const preferred =
          list.find((e) => e.name.toLowerCase() === 'production') ||
          list.find((e) => !e.is_preview) ||
          list[0]
        if (preferred) setEnvironmentId(String(preferred.id))
      })
      .catch((caught) => {
        if (!cancelled) setError(dropErrorMessage(caught))
      })
    return () => {
      cancelled = true
    }
  }, [project.id])

  useEffect(
    () => () => {
      detectionAbortRef.current?.abort()
    },
    []
  )

  const inspectArchive = async (archive: File, signal?: AbortSignal) => {
    const response = await inspectDropArchive({
      throwOnError: true,
      body: { file: archive },
      signal,
    })
    return response.data
  }

  async function detectSelection(
    nextFiles: DropFile[],
    nextRootPage: string,
    runId: number
  ) {
    detectionAbortRef.current?.abort()
    const controller = new AbortController()
    detectionAbortRef.current = controller
    try {
      setStage('packing')
      const result = await prepareAndInspectDrop(
        nextFiles,
        nextRootPage || undefined,
        (archive) => inspectArchive(archive, controller.signal),
        {
          signal: controller.signal,
          onArchivePrepared: (archive) => {
            if (detectionRunRef.current !== runId) return
            setPreparedArchive(archive)
            setStage('detecting')
          },
        }
      )
      if (detectionRunRef.current !== runId) return
      setPreparedArchive(result.archive)
      setInspection(result.inspection)
      const current = (project.directory || '.').replace(/^\.\/+/, '') || '.'
      const match = result.inspection.candidates.findIndex(
        (candidate) => (candidate.directory || '.') === current
      )
      setCandidateIndex(String(match >= 0 ? match : 0))
      setStage('idle')
    } catch (caught) {
      if (detectionRunRef.current !== runId) return
      setError(dropErrorMessage(caught))
      setPreparedArchive(null)
      setInspection(null)
      setStage('idle')
    } finally {
      if (detectionAbortRef.current === controller) {
        detectionAbortRef.current = null
      }
    }
  }

  const select = (next: DropFile[]) => {
    const runId = ++detectionRunRef.current
    detectionAbortRef.current?.abort()
    detectionAbortRef.current = null
    setFiles(next)
    setError(null)
    setPreparedArchive(null)
    setInspection(null)
    setCandidateIndex('0')
    setStage('idle')
    const candidates = htmlRootCandidates(next)
    const hasIndex = candidates.some((p) => p.toLowerCase() === 'index.html')
    const nextRootPage = hasIndex ? '' : candidates[0] || ''
    setRootPage(nextRootPage)
    if (next.length > 0) {
      void detectSelection(next, nextRootPage, runId)
    }
  }

  const run = async () => {
    if (files.length === 0 || isBusy || !environmentId) return
    setError(null)
    try {
      if (!preparedArchive || !inspection) {
        const runId = ++detectionRunRef.current
        await detectSelection(files, rootPage, runId)
        return
      }

      if (!candidate) throw new Error('Choose a detected project')

      // Only persist directory/preset, and only when they actually changed.
      const currentDirectory =
        (project.directory || '.').replace(/^\.\/+/, '') || '.'
      const nextDirectory = candidate.directory || '.'
      if (
        nextDirectory !== currentDirectory ||
        candidate.preset !== project.preset
      ) {
        setStage('saving')
        await updateProjectSettings({
          throwOnError: true,
          path: { project_id: project.id },
          body: { directory: nextDirectory, preset: candidate.preset },
        })
      }

      const targetEnvironment = Number(environmentId)

      if (candidate.isStatic) {
        setStage('uploading')
        const uploadResponse = await uploadStaticBundle({
          throwOnError: true,
          path: { project_id: project.id },
          body: { file: preparedArchive },
        })
        const bundle = uploadResponse.data
        if (!bundle) throw new Error('Static upload returned no bundle')

        setStage('deploying')
        await deployFromStatic({
          throwOnError: true,
          path: { project_id: project.id, environment_id: targetEnvironment },
          body: { static_bundle_id: bundle.id },
        })
        navigate(`/projects/${project.slug}/deployments?autoRefresh=true`)
        return
      }

      setStage('uploading')
      const response = await deployFromUploadedSource({
        throwOnError: true,
        path: { project_id: project.id, environment_id: targetEnvironment },
        body: { file: preparedArchive },
      })
      const deployment = response.data
      if (!deployment) throw new Error('Source deployment returned no result')
      navigate(`/projects/${project.slug}/deployments/${deployment.id}`)
    } catch (caught) {
      setError(dropErrorMessage(caught))
      setStage('idle')
    }
  }

  return (
    <div className="space-y-6">
      <header className="border-b pb-6">
        <p className="mb-2 font-mono text-xs uppercase tracking-[0.28em] text-primary">
          Repository optional
        </p>
        {/* h2, not h1 — the project shell already renders the page's h1. */}
        <h2 className="text-balance text-xl font-semibold tracking-tight">
          Upload new source
        </h2>
        <p className="mt-3 max-w-2xl text-sm leading-6 text-muted-foreground">
          Drop a folder or ZIP to build and deploy a new version of{' '}
          <span className="font-medium text-foreground">{project.name}</span>.
          Only the build directory and preset can change here — ports, resources
          and environment variables stay as configured.
        </p>
      </header>

      <div className="grid gap-6">
        <>
          {files.length === 0 ? (
            <div className="animate-in fade-in-0 zoom-in-95 motion-reduce:animate-none">
              <DropZone
                files={files}
                onSelect={select}
                onError={setError}
                disabled={isBusy}
              />
            </div>
          ) : (
            <aside className="relative flex min-h-[28rem] animate-in flex-col rounded-[2rem] border bg-card p-6 shadow-sm fade-in-0 motion-reduce:animate-none sm:p-7">
              <button
                type="button"
                className="absolute right-5 top-5 z-10 rounded-full border bg-background p-2 text-muted-foreground transition-colors hover:text-foreground"
                onClick={() => select([])}
                disabled={['saving', 'uploading', 'deploying'].includes(stage)}
                aria-label="Clear selected files"
              >
                <X className="size-4" />
              </button>
              <div className="flex items-center justify-between border-b pb-5">
                <div>
                  <p className="font-mono text-[0.68rem] uppercase tracking-[0.24em] text-muted-foreground">
                    Deployment card
                  </p>
                  <h2 className="mt-1 text-xl font-semibold">
                    Configure upload
                  </h2>
                </div>
                <div
                  className={cn(
                    'mr-12 size-2.5 rounded-full',
                    files.length ? 'bg-emerald-500' : 'bg-muted-foreground/30'
                  )}
                />
              </div>

              <div className="flex-1 space-y-6 py-6">
                <div className="space-y-2">
                  <Label>Environment</Label>
                  <Select
                    value={environmentId}
                    onValueChange={setEnvironmentId}
                    disabled={isBusy}
                  >
                    <SelectTrigger>
                      <SelectValue placeholder="Select environment..." />
                    </SelectTrigger>
                    <SelectContent>
                      {environments.map((env) => (
                        <SelectItem key={env.id} value={String(env.id)}>
                          {env.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>

                {!isArchive && !hasRootIndex && htmlPages.length > 0 && (
                  <div className="space-y-2">
                    <Label>Root page</Label>
                    <Select
                      value={rootPage}
                      onValueChange={(value) => {
                        const runId = ++detectionRunRef.current
                        setRootPage(value)
                        setPreparedArchive(null)
                        setInspection(null)
                        setError(null)
                        void detectSelection(files, value, runId)
                      }}
                      disabled={isBusy}
                    >
                      <SelectTrigger>
                        <SelectValue placeholder="Choose the landing page" />
                      </SelectTrigger>
                      <SelectContent>
                        {htmlPages.map((page) => (
                          <SelectItem key={page} value={page}>
                            {page}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                )}

                {inspection && inspection.candidates.length > 1 && (
                  <div className="space-y-2">
                    <Label>Detected projects</Label>
                    <DetectedPresetGrid
                      candidates={inspection.candidates}
                      selectedIndex={Number(candidateIndex)}
                      onSelect={(index) => setCandidateIndex(String(index))}
                      disabled={isBusy}
                    />
                  </div>
                )}

                {(!inspection || inspection.candidates.length === 1) && (
                  <DetectedPresetCard
                    candidate={candidate}
                    isDetecting={stage === 'packing' || stage === 'detecting'}
                    phase={stage === 'packing' ? 'packing' : 'detecting'}
                  />
                )}

                <div className="rounded-xl border bg-muted/35 p-4 text-sm">
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-muted-foreground">
                      Build directory
                    </span>
                    <span className="font-medium">
                      {candidate
                        ? candidate.directory || '.'
                        : project.directory || '.'}
                    </span>
                  </div>
                  <div className="mt-3 flex items-center justify-between gap-3">
                    <span className="text-muted-foreground">Preset</span>
                    <span className="font-medium">
                      {candidate ? candidate.label : 'Pending detection'}
                    </span>
                  </div>
                  <div className="mt-3 flex items-center justify-between gap-3">
                    <span className="text-muted-foreground">Upload limit</span>
                    <span className="font-medium">
                      500 MB ZIP / 100 MB folder
                    </span>
                  </div>
                </div>

                {error && (
                  <div
                    role="alert"
                    className="rounded-xl border border-destructive/30 bg-destructive/5 p-4 text-sm leading-6 text-destructive"
                  >
                    {error}
                  </div>
                )}
              </div>

              <Button
                size="lg"
                className="h-12 w-full"
                disabled={files.length === 0 || isBusy || !environmentId}
                onClick={run}
              >
                {isBusy ? (
                  <Loader2 className="mr-2 size-4 animate-spin" />
                ) : (
                  <UploadCloud className="mr-2 size-4" />
                )}
                {stageLabel(stage, candidate?.label, files.length > 0)}
              </Button>
            </aside>
          )}
        </>
      </div>
    </div>
  )
}
