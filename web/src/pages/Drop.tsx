import {
  createProject,
  deleteProject,
  deployFromUploadedSource,
  deployFromStatic,
  getEnvironments,
  inspectDropArchive,
  type DropInspectionResponse,
  type EnvironmentResponse,
  type ProjectResponse,
} from '@/api/client'
import { DropZone } from '@/components/drop/DropZone'
import { PageContainer } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  formatDetectedProjectLabel,
  htmlRootCandidates,
  isDropArchive,
  prepareDrop,
  type DropFile,
} from '@/lib/drop-archive'
import { dropErrorMessage, inferredProjectName } from '@/lib/drop-files'
import { ensureDropProjectName } from '@/lib/drop-project-name'
import { cn } from '@/lib/utils'
import { ArrowRight, Check, Loader2, RotateCcw, UploadCloud } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router'

type DropStage =
  | 'idle'
  | 'packing'
  | 'detecting'
  | 'creating'
  | 'uploading'
  | 'deploying'
  | 'done'

function stageLabel(stage: DropStage): string {
  switch (stage) {
    case 'packing':
      return 'Packing files locally'
    case 'creating':
      return 'Creating project'
    case 'detecting':
      return 'Detecting preset'
    case 'uploading':
      return 'Uploading project'
    case 'deploying':
      return 'Starting deployment'
    case 'done':
      return 'Deployment started'
    default:
      return 'Ready to deploy'
  }
}

export function Drop() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [files, setFiles] = useState<DropFile[]>([])
  const [projectName, setProjectName] = useState('')
  const [nameWasEdited, setNameWasEdited] = useState(false)
  const [rootPage, setRootPage] = useState('')
  const [stage, setStage] = useState<DropStage>('idle')
  const [error, setError] = useState<string | null>(null)
  const [project, setProject] = useState<ProjectResponse | null>(null)
  const [environment, setEnvironment] = useState<EnvironmentResponse | null>(
    null
  )
  const [preparedArchive, setPreparedArchive] = useState<File | null>(null)
  const [inspection, setInspection] = useState<DropInspectionResponse | null>(
    null
  )
  const [selectedCandidateIndex, setSelectedCandidateIndex] = useState('0')

  usePageTitle('Drop')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'Drop' },
    ])
  }, [setBreadcrumbs])

  const normalizedCandidates = useMemo(() => htmlRootCandidates(files), [files])
  const hasRootIndex = normalizedCandidates.some(
    (path) => path.toLowerCase() === 'index.html'
  )
  const isArchive = files.length === 1 && isDropArchive(files[0].file.name)
  const isBusy = !['idle', 'done'].includes(stage)

  const setSelection = (nextFiles: DropFile[]) => {
    setFiles(nextFiles)
    setError(null)
    setProject(null)
    setEnvironment(null)
    setPreparedArchive(null)
    setInspection(null)
    setSelectedCandidateIndex('0')
    setStage('idle')
    const candidates = htmlRootCandidates(nextFiles)
    const hasIndex = candidates.some(
      (path) => path.toLowerCase() === 'index.html'
    )
    setRootPage(hasIndex ? '' : candidates[0] || '')
    if (!nameWasEdited) {
      setProjectName(ensureDropProjectName(inferredProjectName(nextFiles)))
    }
  }

  const reset = () => {
    setFiles([])
    setProjectName('')
    setNameWasEdited(false)
    setRootPage('')
    setStage('idle')
    setError(null)
    setProject(null)
    setEnvironment(null)
    setPreparedArchive(null)
    setInspection(null)
    setSelectedCandidateIndex('0')
  }

  const deploy = async () => {
    if (files.length === 0 || isBusy) return

    let createdProject: ProjectResponse | null = null
    let deploymentAccepted = false
    const normalizedProjectName = ensureDropProjectName(projectName)
    setProjectName(normalizedProjectName)
    setError(null)
    try {
      let archive = preparedArchive
      let detected = inspection
      if (!archive || !detected) {
        setStage('packing')
        const prepared = await prepareDrop(files, rootPage || undefined)
        archive = prepared.file
        setPreparedArchive(archive)

        setStage('detecting')
        const inspectResponse = await inspectDropArchive({
          throwOnError: true,
          body: { file: archive },
        })
        detected = inspectResponse.data
        if (!detected) throw new Error('Preset detection returned no result')
        setInspection(detected)
        setSelectedCandidateIndex('0')
        if (!nameWasEdited) {
          setProjectName(ensureDropProjectName(detected.suggestedName))
        }
        setStage('idle')
        return
      }

      const candidate = detected.candidates[Number(selectedCandidateIndex)]
      if (!candidate) throw new Error('Choose a detected project preset')

      setStage('creating')
      const projectResult = await createProject({
        throwOnError: true,
        body: {
          name: normalizedProjectName,
          directory: candidate.directory,
          main_branch: 'main',
          // Static-file projects do not build this preset, but project creation
          // still validates the catalog slug. Match the existing manual-project
          // flow and use a known built-in preset as metadata.
          preset: candidate.preset,
          source_type: candidate.isStatic ? 'static_files' : 'uploaded_source',
          project_type: candidate.isStatic ? 'static' : 'server',
          automatic_deploy: false,
          storage_service_ids: [],
        },
      })
      createdProject = projectResult.data
      if (!createdProject) throw new Error('Temps created no project record')

      const environmentsResult = await getEnvironments({
        throwOnError: true,
        path: { project_id: createdProject.id },
      })
      const environments = environmentsResult.data || []
      const targetEnvironment =
        environments.find((item) => item.name.toLowerCase() === 'production') ||
        environments.find((item) => !item.is_preview) ||
        environments[0]
      if (!targetEnvironment)
        throw new Error('The project has no deployment environment')

      setStage('uploading')
      const body = new FormData()
      body.append('file', archive)
      if (!candidate.isStatic) {
        const sourceResponse = await deployFromUploadedSource({
          throwOnError: true,
          path: {
            project_id: createdProject.id,
            environment_id: targetEnvironment.id,
          },
          body: { file: archive },
        })
        const sourceDeployment = sourceResponse.data
        if (!sourceDeployment)
          throw new Error('Source deployment returned no result')
        deploymentAccepted = true
        navigate(
          `/projects/${createdProject.slug}/deployments/${sourceDeployment.id}`
        )
        return
      }
      const uploadResponse = await fetch(
        `/api/projects/${createdProject.id}/upload/static`,
        { method: 'POST', credentials: 'include', body }
      )
      if (!uploadResponse.ok) {
        const problem = (await uploadResponse.json().catch(() => null)) as {
          detail?: string
        } | null
        throw new Error(
          problem?.detail || `Upload failed (${uploadResponse.status})`
        )
      }
      const bundle = (await uploadResponse.json()) as { id: number }

      setStage('deploying')
      await deployFromStatic({
        throwOnError: true,
        path: {
          project_id: createdProject.id,
          environment_id: targetEnvironment.id,
        },
        body: { static_bundle_id: bundle.id },
      })
      deploymentAccepted = true
      setProject(createdProject)
      setEnvironment(targetEnvironment)
      setStage('done')
    } catch (caught) {
      let message = dropErrorMessage(caught)
      if (createdProject && !deploymentAccepted) {
        try {
          await deleteProject({
            throwOnError: true,
            path: { id: createdProject.id },
          })
          message += ' The incomplete project was removed.'
        } catch (cleanupError) {
          message += ` Cleanup also failed: ${dropErrorMessage(cleanupError)}`
        }
      }
      setError(message)
      setStage('idle')
    }
  }

  if (stage === 'done' && project && environment) {
    return (
      <PageContainer
        width="wide"
        innerClassName="min-h-[calc(100vh-8rem)] flex items-center"
      >
        <div className="relative w-full overflow-hidden rounded-[2rem] border bg-card p-8 shadow-sm sm:p-12">
          <div className="absolute inset-0 bg-[radial-gradient(circle_at_80%_10%,hsl(var(--primary)/0.13),transparent_35%)]" />
          <div className="relative max-w-3xl">
            <div className="mb-8 flex size-14 items-center justify-center rounded-2xl bg-emerald-500 text-white shadow-lg shadow-emerald-500/20">
              <Check className="size-7" />
            </div>
            <p className="mb-3 font-mono text-xs uppercase tracking-[0.28em] text-emerald-600 dark:text-emerald-400">
              Drop accepted
            </p>
            <h1 className="text-balance text-4xl font-semibold tracking-tight sm:text-6xl">
              {project.name} is on its way live.
            </h1>
            <p className="mt-5 max-w-2xl text-lg leading-8 text-muted-foreground">
              Temps accepted the bundle and started the deployment. The project
              page has the live build log and final status.
            </p>
            <div className="mt-9 flex flex-wrap gap-3">
              <Button asChild size="lg">
                <Link to={`/projects/${project.slug}`}>
                  Watch deployment <ArrowRight className="ml-2 size-4" />
                </Link>
              </Button>
              <Button asChild size="lg" variant="outline">
                <a href={environment.main_url} target="_blank" rel="noreferrer">
                  Open URL
                </a>
              </Button>
              <Button size="lg" variant="ghost" onClick={reset}>
                <RotateCcw className="mr-2 size-4" /> Drop another
              </Button>
            </div>
          </div>
        </div>
      </PageContainer>
    )
  }

  return (
    <PageContainer width="wide" innerClassName="space-y-8">
      <header className="grid gap-6 border-b pb-8 lg:grid-cols-[1fr_auto] lg:items-end">
        <div>
          <p className="mb-3 font-mono text-xs uppercase tracking-[0.28em] text-primary">
            Repository optional
          </p>
          <h1 className="text-balance text-4xl font-semibold tracking-tight sm:text-5xl">
            Drop files. Get a deployment.
          </h1>
          <p className="mt-4 max-w-2xl text-base leading-7 text-muted-foreground">
            Ship a static site or application without Git or a CLI. Temps
            detects the preset, creates a project, and starts the right build
            pipeline in one pass.
          </p>
        </div>
        <div className="flex gap-6 font-mono text-xs uppercase tracking-wider text-muted-foreground">
          <span>01 Select</span>
          <span>02 Name</span>
          <span>03 Deploy</span>
        </div>
      </header>

      <div className="grid gap-6 lg:grid-cols-[minmax(0,1.6fr)_minmax(20rem,0.8fr)]">
        <DropZone
          files={files}
          onSelect={setSelection}
          onError={setError}
          disabled={isBusy}
        />

        <aside className="flex flex-col rounded-[2rem] border bg-card p-6 shadow-sm sm:p-7">
          <div className="flex items-center justify-between border-b pb-5">
            <div>
              <p className="font-mono text-[0.68rem] uppercase tracking-[0.24em] text-muted-foreground">
                Deployment card
              </p>
              <h2 className="mt-1 text-xl font-semibold">Configure drop</h2>
            </div>
            <div
              className={cn(
                'size-2.5 rounded-full',
                files.length ? 'bg-emerald-500' : 'bg-muted-foreground/30'
              )}
            />
          </div>

          <div className="flex-1 space-y-6 py-6">
            <div className="space-y-2">
              <Label htmlFor="drop-name">Project name</Label>
              <Input
                id="drop-name"
                value={projectName}
                placeholder="my-static-site"
                disabled={isBusy}
                onChange={(event) => {
                  setProjectName(event.target.value)
                  setNameWasEdited(true)
                }}
                onBlur={() =>
                  setProjectName(ensureDropProjectName(projectName))
                }
              />
            </div>

            {!isArchive && !hasRootIndex && normalizedCandidates.length > 0 && (
              <div className="space-y-2">
                <Label>Root page</Label>
                <Select
                  value={rootPage}
                  onValueChange={setRootPage}
                  disabled={isBusy}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Choose the landing page" />
                  </SelectTrigger>
                  <SelectContent>
                    {normalizedCandidates.map((candidate) => (
                      <SelectItem key={candidate} value={candidate}>
                        {candidate}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs leading-5 text-muted-foreground">
                  Temps adds a small index redirect; your selected file stays in
                  place.
                </p>
              </div>
            )}

            {inspection && (
              <div className="space-y-2">
                <Label>Detected project</Label>
                <Select
                  value={selectedCandidateIndex}
                  onValueChange={setSelectedCandidateIndex}
                  disabled={isBusy}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Choose a project" />
                  </SelectTrigger>
                  <SelectContent>
                    {inspection.candidates.map((candidate, index) => (
                      <SelectItem
                        key={`${candidate.directory}:${candidate.preset}`}
                        value={String(index)}
                      >
                        {formatDetectedProjectLabel(
                          candidate.label,
                          candidate.directory
                        )}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs leading-5 text-muted-foreground">
                  {
                    inspection.candidates[Number(selectedCandidateIndex)]
                      ?.reason
                  }
                </p>
              </div>
            )}

            <div className="rounded-xl border bg-muted/35 p-4 text-sm">
              <div className="flex items-center justify-between gap-3">
                <span className="text-muted-foreground">Source</span>
                <span className="font-medium">
                  {inspection
                    ? inspection.candidates[Number(selectedCandidateIndex)]
                        ?.label
                    : 'Pending detection'}
                </span>
              </div>
              <div className="mt-3 flex items-center justify-between gap-3">
                <span className="text-muted-foreground">Git connection</span>
                <span className="font-medium">None</span>
              </div>
              <div className="mt-3 flex items-center justify-between gap-3">
                <span className="text-muted-foreground">Upload limit</span>
                <span className="font-medium">500 MB ZIP / 100 MB folder</span>
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
            disabled={files.length === 0 || isBusy}
            onClick={deploy}
          >
            {isBusy ? (
              <Loader2 className="mr-2 size-4 animate-spin" />
            ) : (
              <UploadCloud className="mr-2 size-4" />
            )}
            {inspection
              ? stageLabel(stage)
              : isBusy
                ? stageLabel(stage)
                : 'Detect preset'}
          </Button>
          <p className="mt-3 text-center text-xs text-muted-foreground">
            Failed setup is rolled back automatically.
          </p>
        </aside>
      </div>

    </PageContainer>
  )
}
