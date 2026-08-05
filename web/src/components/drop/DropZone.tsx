import { Button } from '@/components/ui/button'
import { htmlRootCandidates, isDropArchive, type DropFile } from '@/lib/drop-archive'
import { filesFromDrop, filesFromInput } from '@/lib/drop-files'
import { cn } from '@/lib/utils'
import {
  FileArchive,
  FileCode2,
  FolderOpen,
  PackageOpen,
  UploadCloud,
  X,
} from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'

interface DropZoneProps {
  files: DropFile[]
  /** Called with the new selection, or `[]` when the user clears it. */
  onSelect: (files: DropFile[]) => void
  onError: (message: string) => void
  disabled?: boolean
}

/**
 * The drag-and-drop surface shared by `/drop` (new project) and
 * `/projects/:slug/drop` (deploy into an existing project). Both accept the
 * same three shapes — a project folder, a single HTML file, or a `.zip` —
 * because a user who learned the gesture on one page should not discover the
 * other silently accepts less.
 *
 * `webkitdirectory` is set imperatively: React does not recognise it as a JSX
 * prop, and hardcoding it would break the "choose a single file" input.
 */
export function DropZone({
  files,
  onSelect,
  onError,
  disabled = false,
}: DropZoneProps) {
  const folderInputRef = useRef<HTMLInputElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [isDragging, setIsDragging] = useState(false)
  const inputId = useId()

  useEffect(() => {
    folderInputRef.current?.setAttribute('webkitdirectory', '')
  }, [])

  const htmlPages = htmlRootCandidates(files)
  const isArchive = files.length === 1 && isDropArchive(files[0].file.name)
  const totalBytes = files.reduce((sum, item) => sum + item.file.size, 0)

  return (
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
        if (!disabled) setIsDragging(true)
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
        if (disabled) return
        try {
          onSelect(await filesFromDrop(event))
        } catch (caught) {
          onError(
            caught instanceof Error
              ? caught.message
              : 'Those files could not be read'
          )
        }
      }}
    >
      <div className="absolute inset-0 bg-gradient-to-b from-background/20 to-background/80" />
      <div className="relative flex min-h-[28rem] flex-col items-center justify-center px-6 text-center">
        {files.length === 0 ? (
          <>
            <div className="mb-7 flex size-24 items-center justify-center rounded-[2rem] border bg-background shadow-xl transition-transform duration-300 group-hover:-translate-y-1">
              <UploadCloud className="size-10 text-primary" strokeWidth={1.5} />
            </div>
            <h2 className="text-2xl font-semibold tracking-tight">
              Drag your project here
            </h2>
            <p className="mt-3 max-w-md text-sm leading-6 text-muted-foreground">
              A project folder, one HTML file, or a .zip archive. Folders are
              packaged locally before upload.
            </p>
            <div className="mt-7 flex flex-wrap justify-center gap-3">
              <Button
                disabled={disabled}
                onClick={() => folderInputRef.current?.click()}
              >
                <FolderOpen className="mr-2 size-4" /> Choose folder
              </Button>
              <Button
                variant="outline"
                disabled={disabled}
                onClick={() => fileInputRef.current?.click()}
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
              onClick={() => onSelect([])}
              disabled={disabled}
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
                ` · ${htmlPages.length} HTML page${htmlPages.length === 1 ? '' : 's'}`}
            </p>
          </>
        )}
      </div>

      <input
        ref={fileInputRef}
        id={`${inputId}-file`}
        type="file"
        className="hidden"
        accept=".html,.htm,.zip,application/zip"
        onChange={(event) => onSelect(filesFromInput(event.target.files))}
      />
      <input
        ref={folderInputRef}
        id={`${inputId}-folder`}
        type="file"
        className="hidden"
        multiple
        onChange={(event) => onSelect(filesFromInput(event.target.files))}
      />
    </section>
  )
}
