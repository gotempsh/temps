import type { ProjectResponse, SourceMapResponse } from '@/api/client'
import {
  deleteReleaseSourceMapsMutation,
  deleteSourceMapMutation,
  listReleasesOptions,
  listSourceMapsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ChevronRight,
  FileCode2,
  Loader2,
  MapPin,
  Trash2,
  Upload,
} from 'lucide-react'
import { useRef, useState } from 'react'
import { toast } from 'sonner'
import { TimeAgo } from '../utils/TimeAgo'

// --- API function for multipart upload (generated client can't handle multipart) ---

async function uploadSourceMapFile(
  projectId: number,
  release: string,
  file: File,
  filePath: string,
  dist?: string,
): Promise<SourceMapResponse> {
  const formData = new FormData()
  formData.append('file', file)
  formData.append('file_path', filePath)
  if (dist) formData.append('dist', dist)

  const response = await fetch(
    `/api/projects/${projectId}/releases/${encodeURIComponent(release)}/source-maps`,
    {
      method: 'POST',
      body: formData,
      credentials: 'include',
    },
  )

  if (!response.ok) {
    const text = await response.text()
    throw new Error(text || 'Failed to upload source map')
  }

  return response.json()
}

// --- Helper functions ---

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return `${parseFloat((bytes / Math.pow(k, i)).toFixed(1))} ${sizes[i]}`
}

// --- Component ---

interface SourceMapsProps {
  project: ProjectResponse
}

export function SourceMaps({ project }: SourceMapsProps) {
  const queryClient = useQueryClient()
  const [selectedRelease, setSelectedRelease] = useState<string | null>(null)
  const [isUploadOpen, setIsUploadOpen] = useState(false)
  const [deleteConfirm, setDeleteConfirm] = useState<{
    type: 'release' | 'file'
    release?: string
    fileId?: number
    fileName?: string
  } | null>(null)

  // Fetch releases using generated options
  const releasesQueryOpts = listReleasesOptions({
    path: { project_id: project.id },
  })
  const {
    data: releasesData,
    isLoading: isLoadingReleases,
  } = useQuery({
    ...releasesQueryOpts,
  })

  // Fetch source maps for selected release using generated options
  const mapsQueryOpts = listSourceMapsOptions({
    path: { project_id: project.id, release: selectedRelease ?? '' },
  })
  const {
    data: mapsData,
    isLoading: isLoadingMaps,
  } = useQuery({
    ...mapsQueryOpts,
    enabled: !!selectedRelease,
  })

  // Delete release mutation
  const deleteReleaseMutation = useMutation({
    ...deleteReleaseSourceMapsMutation(),
    onSuccess: (data, variables) => {
      toast.success(
        `Deleted ${data.deleted} source map(s) for release "${variables.path.release}"`,
      )
      queryClient.invalidateQueries({
        queryKey: releasesQueryOpts.queryKey,
      })
      if (selectedRelease === variables.path.release) {
        setSelectedRelease(null)
      }
    },
    onError: () => {
      toast.error('Failed to delete source maps')
    },
  })

  // Delete single source map mutation
  const deleteFileMutation = useMutation({
    ...deleteSourceMapMutation(),
    onSuccess: () => {
      toast.success('Source map deleted')
      if (selectedRelease) {
        queryClient.invalidateQueries({
          queryKey: mapsQueryOpts.queryKey,
        })
      }
      queryClient.invalidateQueries({
        queryKey: releasesQueryOpts.queryKey,
      })
    },
    onError: () => {
      toast.error('Failed to delete source map')
    },
  })

  const releases = releasesData?.releases ?? []

  const handleConfirmDelete = () => {
    if (!deleteConfirm) return
    if (deleteConfirm.type === 'release' && deleteConfirm.release) {
      deleteReleaseMutation.mutate({
        path: { project_id: project.id, release: deleteConfirm.release },
      })
    } else if (deleteConfirm.type === 'file' && deleteConfirm.fileId) {
      deleteFileMutation.mutate({
        path: { project_id: project.id, source_map_id: deleteConfirm.fileId },
      })
    }
    setDeleteConfirm(null)
  }

  return (
    <div className="space-y-6">
      {/* Header with upload button */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-lg font-semibold">Source Maps</h3>
          <p className="text-sm text-muted-foreground">
            Upload source maps to get readable stack traces in error reports.
            Source maps are automatically captured during deployments.
          </p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setIsUploadOpen(true)}
        >
          <Upload className="h-4 w-4 mr-2" />
          Upload Source Map
        </Button>
      </div>

      {/* CLI instructions */}
      <Card>
        <CardHeader>
          <CardTitle className="text-sm">
            Upload via sentry-cli
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            <p className="text-sm text-muted-foreground">
              You can use the standard <code className="text-xs bg-muted px-1 py-0.5 rounded">sentry-cli</code> to upload source maps.
              Use your DSN public key as the auth token:
            </p>
            <div className="flex items-center gap-2">
              <code className="text-xs bg-muted px-3 py-2 rounded font-mono flex-1 block">
                SENTRY_URL={window.location.origin} sentry-cli sourcemaps upload --auth-token YOUR_DSN_KEY --org temps --project {project.slug} ./dist
              </code>
              <CopyButton
                value={`SENTRY_URL=${window.location.origin} sentry-cli sourcemaps upload --auth-token YOUR_DSN_KEY --org temps --project ${project.slug} ./dist`}
                className="h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md shrink-0"
              />
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Releases list */}
      {isLoadingReleases ? (
        <div className="space-y-3">
          {['rel-sk-1', 'rel-sk-2', 'rel-sk-3'].map((key) => (
            <Skeleton key={key} className="h-16" />
          ))}
        </div>
      ) : releases.length === 0 ? (
        <EmptyState
          icon={MapPin}
          title="No source maps uploaded"
          description="Upload source maps to symbolicate stack traces in error reports. Source maps are automatically captured during deployments when available."
        />
      ) : (
        <div className="space-y-3">
          {releases.map((release) => (
            <ReleaseCard
              key={release}
              release={release}
              isSelected={selectedRelease === release}
              onSelect={() =>
                setSelectedRelease(
                  selectedRelease === release ? null : release,
                )
              }
              onDelete={() =>
                setDeleteConfirm({ type: 'release', release })
              }
              maps={
                selectedRelease === release ? mapsData?.source_maps : undefined
              }
              isLoadingMaps={
                selectedRelease === release && isLoadingMaps
              }
              onDeleteFile={(id, name) =>
                setDeleteConfirm({
                  type: 'file',
                  fileId: id,
                  fileName: name,
                })
              }
            />
          ))}
        </div>
      )}

      {/* Upload dialog */}
      <UploadDialog
        open={isUploadOpen}
        onOpenChange={setIsUploadOpen}
        projectId={project.id}
        onSuccess={() => {
          queryClient.invalidateQueries({
            queryKey: releasesQueryOpts.queryKey,
          })
          if (selectedRelease) {
            queryClient.invalidateQueries({
              queryKey: mapsQueryOpts.queryKey,
            })
          }
        }}
      />

      {/* Delete confirmation dialog */}
      <Dialog
        open={!!deleteConfirm}
        onOpenChange={(open) => !open && setDeleteConfirm(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Source Map{deleteConfirm?.type === 'release' ? 's' : ''}</DialogTitle>
            <DialogDescription>
              {deleteConfirm?.type === 'release'
                ? `This will delete all source maps for release "${deleteConfirm.release}". This action cannot be undone.`
                : `This will delete the source map "${deleteConfirm?.fileName}". This action cannot be undone.`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setDeleteConfirm(null)}
            >
              Cancel
            </Button>
            <Button
              type="button"
              variant="destructive"
              onClick={handleConfirmDelete}
              disabled={
                deleteReleaseMutation.isPending ||
                deleteFileMutation.isPending
              }
            >
              {deleteReleaseMutation.isPending ||
              deleteFileMutation.isPending ? (
                <>
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  Deleting...
                </>
              ) : (
                'Delete'
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

// --- Release card component ---

interface ReleaseCardProps {
  release: string
  isSelected: boolean
  onSelect: () => void
  onDelete: () => void
  maps?: SourceMapResponse[]
  isLoadingMaps: boolean
  onDeleteFile: (id: number, name: string) => void
}

function ReleaseCard({
  release,
  isSelected,
  onSelect,
  onDelete,
  maps,
  isLoadingMaps,
  onDeleteFile,
}: ReleaseCardProps) {
  const shortRelease =
    release.length > 40 ? `${release.substring(0, 12)}...` : release

  return (
    <Card>
      <button
        type="button"
        className="flex items-center justify-between px-4 py-3 cursor-pointer hover:bg-muted/50 transition-colors w-full text-left"
        onClick={onSelect}
      >
        <div className="flex items-center gap-3">
          <ChevronRight
            className={`h-4 w-4 text-muted-foreground transition-transform ${
              isSelected ? 'rotate-90' : ''
            }`}
          />
          <div>
            <div className="flex items-center gap-2">
              <span className="font-mono text-sm font-medium">
                {shortRelease}
              </span>
              {release !== shortRelease && (
                <CopyButton
                  value={release}
                  className="h-6 w-6 p-0 hover:bg-accent rounded-md"
                />
              )}
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {maps && (
            <Badge variant="secondary">
              {maps.length} file{maps.length !== 1 ? 's' : ''}
            </Badge>
          )}
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="h-8 w-8 text-muted-foreground hover:text-destructive"
            onClick={(e) => {
              e.stopPropagation()
              onDelete()
            }}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </button>

      {isSelected && (
        <CardContent className="pt-0 pb-4">
          {isLoadingMaps ? (
            <div className="space-y-2">
              {['map-sk-1', 'map-sk-2', 'map-sk-3'].map((key) => (
                <Skeleton key={key} className="h-10" />
              ))}
            </div>
          ) : maps && maps.length > 0 ? (
            <div className="divide-y">
              {maps.map((map) => (
                <SourceMapFileRow
                  key={map.id}
                  map={map}
                  onDelete={() => onDeleteFile(map.id, map.file_path)}
                />
              ))}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground py-2">
              No source maps for this release.
            </p>
          )}
        </CardContent>
      )}
    </Card>
  )
}

// --- Source map file row ---

interface SourceMapFileRowProps {
  map: SourceMapResponse
  onDelete: () => void
}

function SourceMapFileRow({ map, onDelete }: SourceMapFileRowProps) {
  return (
    <div className="flex items-center justify-between py-2 px-2 group hover:bg-muted/30 rounded">
      <div className="flex items-center gap-2 min-w-0">
        <FileCode2 className="h-4 w-4 text-muted-foreground shrink-0" />
        <span className="font-mono text-sm truncate" title={map.file_path}>
          {map.file_path}
        </span>
        <Badge variant="outline" className="text-xs shrink-0">
          {formatBytes(map.size_bytes)}
        </Badge>
      </div>
      <div className="flex items-center gap-2 shrink-0">
        <span className="text-xs text-muted-foreground">
          <TimeAgo date={map.created_at} />
        </span>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-7 w-7 opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-destructive transition-opacity"
          onClick={onDelete}
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  )
}

// --- Upload dialog ---

interface UploadDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  onSuccess: () => void
}

function UploadDialog({
  open,
  onOpenChange,
  projectId,
  onSuccess,
}: UploadDialogProps) {
  const [release, setRelease] = useState('')
  const [filePath, setFilePath] = useState('')
  const [dist, setDist] = useState('')
  const [selectedFile, setSelectedFile] = useState<File | null>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)

  const uploadMutation = useMutation({
    mutationFn: () => {
      if (!selectedFile || !release || !filePath) {
        throw new Error('Missing required fields')
      }
      return uploadSourceMapFile(projectId, release, selectedFile, filePath, dist || undefined)
    },
    onSuccess: () => {
      toast.success('Source map uploaded successfully')
      onOpenChange(false)
      onSuccess()
      resetForm()
    },
    onError: (error: Error) => {
      toast.error(`Upload failed: ${error.message}`)
    },
  })

  function resetForm() {
    setRelease('')
    setFilePath('')
    setDist('')
    setSelectedFile(null)
    if (fileInputRef.current) {
      fileInputRef.current.value = ''
    }
  }

  function handleFileChange(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0]
    if (file) {
      setSelectedFile(file)
      // Auto-fill file path from filename if empty
      if (!filePath) {
        const name = file.name
        const jsName = name.endsWith('.map')
          ? name.slice(0, -4)
          : name
        setFilePath(`~/${jsName}`)
      }
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Upload Source Map</DialogTitle>
          <DialogDescription>
            Upload a .map file to enable readable stack traces for a specific
            release.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="release">Release Version</Label>
            <Input
              id="release"
              placeholder="e.g., 1.0.0 or commit SHA"
              value={release}
              onChange={(e) => setRelease(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              Must match the release version sent by your Sentry SDK.
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="file">Source Map File</Label>
            <Input
              id="file"
              type="file"
              accept=".map,.js.map"
              ref={fileInputRef}
              onChange={handleFileChange}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="filePath">File Path</Label>
            <Input
              id="filePath"
              placeholder="e.g., ~/assets/main.js"
              value={filePath}
              onChange={(e) => setFilePath(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              The URL path of the minified file as it appears in stack traces.
              Uses the ~ prefix convention (e.g., ~/assets/main.js).
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="dist">Distribution (optional)</Label>
            <Input
              id="dist"
              placeholder="e.g., production"
              value={dist}
              onChange={(e) => setDist(e.target.value)}
            />
          </div>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          <Button
            type="button"
            onClick={() => uploadMutation.mutate()}
            disabled={
              uploadMutation.isPending || !release || !selectedFile || !filePath
            }
          >
            {uploadMutation.isPending ? (
              <>
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                Uploading...
              </>
            ) : (
              <>
                <Upload className="h-4 w-4 mr-2" />
                Upload
              </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
