import {
  createProject,
  deleteProject,
  deployFromStatic,
  getEnvironments,
  type EnvironmentResponse,
  type ProjectResponse,
} from '@/api/client'
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
import { ensureDropProjectName } from '@/lib/drop-project-name'
import { cn } from '@/lib/utils'
import {
  ArrowRight,
  Check,
  FileArchive,
  FileCode2,
  FolderOpen,
  Loader2,
  PackageOpen,
  RotateCcw,
  UploadCloud,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router'

type DropStage =
  | 'idle'
  | 'packing'
  | 'detecting'
  | 'creating'
  | 'uploading'
  | 'deploying'
  | 'done'

interface DropPresetCandidate {
  directory: string
  preset: string
  label: string
  confidence: string
  reason: string
  isStatic: boolean
}

interface DropInspection {
  suggestedName: string
  candidates: DropPresetCandidate[]
}

interface LegacyFileSystemEntry {
  isFile: boolean
  isDirectory: boolean
  name: string
}

interface LegacyFileEntry extends LegacyFileSystemEntry {
  file(
    success: (file: File) => void,
    error: (error: DOMException) => void
  ): void
}

interface LegacyDirectoryReader {
  readEntries(
    success: (entries: LegacyFileSystemEntry[]) => void,
    error: (error: DOMException) => void
  ): void
}

interface LegacyDirectoryEntry extends LegacyFileSystemEntry {
  createReader(): LegacyDirectoryReader
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (error && typeof error === 'object') {
    const problem = error as { detail?: string; message?: string }
    return problem.detail || problem.message || 'The drop could not be deployed'
  }
  return 'The drop could not be deployed'
}

function inferredProjectName(files: DropFile[]): string {
  const firstPath = files[0]?.path.replace(/\\/g, '/') || ''
  const parts = firstPath.split('/').filter(Boolean)
  const source = parts.length > 1 ? parts[0] : parts[0] || ''
  return source
    .replace(/\.(tar\.gz|tgz|zip|tar|html?)$/i, '')
    .replace(/[_-]+/g, ' ')
    .trim()
}

async function readDirectoryEntries(
  directory: LegacyDirectoryEntry
): Promise<LegacyFileSystemEntry[]> {
  const reader = directory.createReader()
  const entries: LegacyFileSystemEntry[] = []
  while (true) {
    const batch = await new Promise<LegacyFileSystemEntry[]>(
      (resolve, reject) => reader.readEntries(resolve, reject)
    )
    if (batch.length === 0) return entries
    entries.push(...batch)
  }
}

async function readEntry(
  entry: LegacyFileSystemEntry,
  prefix = ''
): Promise<DropFile[]> {
  const path = prefix ? `${prefix}/${entry.name}` : entry.name
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as LegacyFileEntry).file(resolve, reject)
    )
    return [{ file, path }]
  }
  if (!entry.isDirectory) return []

  const children = await readDirectoryEntries(entry as LegacyDirectoryEntry)
  const nested = await Promise.all(
    children.map((child) => readEntry(child, path))
  )
  return nested.flat()
}

async function filesFromDrop(event: React.DragEvent): Promise<DropFile[]> {
  const items = Array.from(event.dataTransfer.items)
  const entryItems = items
    .map((item) => {
      const getEntry = (
        item as DataTransferItem & {
          webkitGetAsEntry?: () => LegacyFileSystemEntry | null
        }
      ).webkitGetAsEntry
      return getEntry?.call(item) ?? null
    })
    .filter((entry): entry is LegacyFileSystemEntry => entry !== null)

  if (entryItems.length > 0) {
    return (
      await Promise.all(entryItems.map((entry) => readEntry(entry)))
    ).flat()
  }

  return Array.from(event.dataTransfer.files).map((file) => ({
    file,
    path: file.webkitRelativePath || file.name,
  }))
}

function stageLabel(stage: DropStage): string {
  switch (stage) {
    case 'packing':
      return 'Packing files locally'
    case 'creating':
      return 'Creating project'
    case 'detecting':
      return 'Detecting preset'
    case 'uploading':
      return 'Uploading static bundle'
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
  const folderInputRef = useRef<HTMLInputElement>(null)
  const [files, setFiles] = useState<DropFile[]>([])
  const [projectName, setProjectName] = useState('')
  const [nameWasEdited, setNameWasEdited] = useState(false)
  const [rootPage, setRootPage] = useState('')
  const [isDragging, setIsDragging] = useState(false)
  const [stage, setStage] = useState<DropStage>('idle')
  const [error, setError] = useState<string | null>(null)
  const [project, setProject] = useState<ProjectResponse | null>(null)
  const [environment, setEnvironment] = useState<EnvironmentResponse | null>(
    null
  )
  const [preparedArchive, setPreparedArchive] = useState<File | null>(null)
  const [inspection, setInspection] = useState<DropInspection | null>(null)
  const [selectedCandidateIndex, setSelectedCandidateIndex] = useState('0')

  usePageTitle('Drop')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'Drop' },
    ])
  }, [setBreadcrumbs])

  useEffect(() => {
    folderInputRef.current?.setAttribute('webkitdirectory', '')
  }, [])

  const normalizedCandidates = useMemo(() => htmlRootCandidates(files), [files])
  const hasRootIndex = normalizedCandidates.some(
    (path) => path.toLowerCase() === 'index.html'
  )
  const isArchive = files.length === 1 && isDropArchive(files[0].file.name)
  const totalBytes = files.reduce((sum, item) => sum + item.file.size, 0)
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

  const handleInput = (selected: FileList | null) => {
    if (!selected) return
    setSelection(
      Array.from(selected).map((file) => ({
        file,
        path: file.webkitRelativePath || file.name,
      }))
    )
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
        const inspectBody = new FormData()
        inspectBody.append('file', archive)
        const inspectResponse = await fetch('/api/drop/inspect', {
          method: 'POST',
          credentials: 'include',
          body: inspectBody,
        })
        if (!inspectResponse.ok) {
          const problem = (await inspectResponse.json().catch(() => null)) as {
            detail?: string
          } | null
          throw new Error(
            problem?.detail ||
              `Preset detection failed (${inspectResponse.status})`
          )
        }
        detected = (await inspectResponse.json()) as DropInspection
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
        const sourceResponse = await fetch(
          `/api/projects/${createdProject.id}/environments/${targetEnvironment.id}/deploy/source`,
          { method: 'POST', credentials: 'include', body }
        )
        if (!sourceResponse.ok) {
          const problem = (await sourceResponse.json().catch(() => null)) as {
            detail?: string
          } | null
          throw new Error(
            problem?.detail ||
              `Source deployment failed (${sourceResponse.status})`
          )
        }
        const sourceDeployment = (await sourceResponse.json()) as { id: number }
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
      let message = errorMessage(caught)
      if (createdProject && !deploymentAccepted) {
        try {
          await deleteProject({
            throwOnError: true,
            path: { id: createdProject.id },
          })
          message += ' The incomplete project was removed.'
        } catch (cleanupError) {
          message += ` Cleanup also failed: ${errorMessage(cleanupError)}`
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
              Temps is extracting the bundle and starting its static container.
              The project page has the live build log and final status.
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
            Ship a static site without Git or a CLI. Temps creates a project,
            packages folders in your browser, and starts the deployment in one
            pass.
          </p>
        </div>
        <div className="flex gap-6 font-mono text-xs uppercase tracking-wider text-muted-foreground">
          <span>01 Select</span>
          <span>02 Name</span>
          <span>03 Deploy</span>
        </div>
      </header>

      <div className="grid gap-6 lg:grid-cols-[minmax(0,1.6fr)_minmax(20rem,0.8fr)]">
        <section
          className={cn(
            'group relative min-h-[28rem] overflow-hidden rounded-[2rem] border-2 border-dashed transition-all duration-300',
            'bg-[linear-gradient(135deg,hsl(var(--muted)/0.35)_25%,transparent_25%,transparent_50%,hsl(var(--muted)/0.35)_50%,hsl(var(--muted)/0.35)_75%,transparent_75%,transparent)] bg-[length:28px_28px]',
            isDragging
              ? 'scale-[1.01] border-primary bg-primary/5 shadow-2xl shadow-primary/10'
              : 'border-border hover:border-muted-foreground/60'
          )}
          onDragEnter={(event) => {
            event.preventDefault()
            setIsDragging(true)
          }}
          onDragOver={(event) => event.preventDefault()}
          onDragLeave={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget as Node)) {
              setIsDragging(false)
            }
          }}
          onDrop={async (event) => {
            event.preventDefault()
            setIsDragging(false)
            try {
              setSelection(await filesFromDrop(event))
            } catch (caught) {
              setError(errorMessage(caught))
            }
          }}
        >
          <div className="absolute inset-0 bg-gradient-to-b from-background/20 to-background/80" />
          <div className="relative flex min-h-[28rem] flex-col items-center justify-center px-6 text-center">
            {files.length === 0 ? (
              <>
                <div className="mb-7 flex size-24 items-center justify-center rounded-[2rem] border bg-background shadow-xl transition-transform duration-300 group-hover:-translate-y-1">
                  <UploadCloud
                    className="size-10 text-primary"
                    strokeWidth={1.5}
                  />
                </div>
                <h2 className="text-2xl font-semibold tracking-tight">
                  Drag your site here
                </h2>
                <p className="mt-3 max-w-md text-sm leading-6 text-muted-foreground">
                  A folder, one HTML file, or a .zip, .tar, .tar.gz, or .tgz
                  archive. Files are packaged locally before upload.
                </p>
                <div className="mt-7 flex flex-wrap justify-center gap-3">
                  <Button onClick={() => folderInputRef.current?.click()}>
                    <FolderOpen className="mr-2 size-4" /> Choose folder
                  </Button>
                  <Button
                    variant="outline"
                    onClick={() =>
                      document.getElementById('drop-file-input')?.click()
                    }
                  >
                    <FileArchive className="mr-2 size-4" /> Choose file
                  </Button>
                </div>
              </>
            ) : (
              <>
                <button
                  type="button"
                  className="absolute right-5 top-5 rounded-full border bg-background p-2 text-muted-foreground transition-colors hover:text-foreground"
                  onClick={reset}
                  aria-label="Clear selected files"
                >
                  <X className="size-4" />
                </button>
                <div className="mb-7 flex size-24 items-center justify-center rounded-[2rem] bg-foreground text-background shadow-xl">
                  {isArchive ? (
                    <PackageOpen className="size-10" strokeWidth={1.5} />
                  ) : (
                    <FileCode2 className="size-10" strokeWidth={1.5} />
                  )}
                </div>
                <h2 className="max-w-xl truncate text-2xl font-semibold tracking-tight">
                  {files.length === 1
                    ? files[0].file.name
                    : `${files.length} files ready`}
                </h2>
                <p className="mt-3 font-mono text-xs uppercase tracking-wider text-muted-foreground">
                  {(totalBytes / 1024).toLocaleString(undefined, {
                    maximumFractionDigits: 1,
                  })}{' '}
                  KB
                  {!isArchive &&
                    ` · ${normalizedCandidates.length} HTML page${normalizedCandidates.length === 1 ? '' : 's'}`}
                </p>
              </>
            )}
          </div>
        </section>

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
                <span className="font-medium">500 MB</span>
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

      <input
        id="drop-file-input"
        type="file"
        className="hidden"
        accept=".html,.htm,.zip,.tar,.tar.gz,.tgz,application/zip,application/gzip"
        onChange={(event) => handleInput(event.target.files)}
      />
      <input
        ref={folderInputRef}
        type="file"
        className="hidden"
        multiple
        onChange={(event) => handleInput(event.target.files)}
      />
    </PageContainer>
  )
}
