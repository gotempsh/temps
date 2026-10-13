// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { exec } from '@/api/client'
import { FileText, Loader2, Save } from 'lucide-react'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  readFileOptions,
  readFileQueryKey,
  statPathOptions,
  writeFileMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { problemDetail } from '@/lib/api-problem'
import {
  DEFAULT_FILE_MODE,
  MAX_EDITABLE_BYTES,
  type FileProperties,
  decodeFile,
  defaultFilePath,
  encodeText,
  filePropertiesArgv,
  filesUnavailableReason,
  formatFileMode,
  isEditableFile,
  parseFileProperties,
  propertiesAfterSave,
  saveRisks,
  writeMode,
  validateSandboxPath,
} from './sandbox-files'

/** Cache key for a file's permission bits (read via `stat`, see the card). */
function filePropertiesQueryKey(sandboxId: string, path: string) {
  return [{ _id: 'sandboxFileProperties', sandboxId, path }] as const
}

const pathFormSchema = z.object({
  path: z.string().superRefine((value, context) => {
    const problem = validateSandboxPath(value)
    if (problem) context.addIssue({ code: 'custom', message: problem })
  }),
})
type PathFormValues = z.infer<typeof pathFormSchema>

interface SandboxFilesCardProps {
  sandboxId: string
  workDir: string
  status: string
  lifecycle?: string
}

/**
 * Open, edit and save one text file in a sandbox (`fs/read`, `fs/write`).
 *
 * There is no directory-listing endpoint yet, so files are addressed by
 * path rather than browsed.
 */
export function SandboxFilesCard({
  sandboxId,
  workDir,
  status,
  lifecycle,
}: SandboxFilesCardProps) {
  const [openPath, setOpenPath] = useState<string | null>(null)
  // Bumped when the operator re-opens the open path, after the fresh stat and
  // contents have arrived, so the editor restarts from what is in the
  // sandbox. Background updates (a save, a cache refresh) never restart it.
  const [generation, setGeneration] = useState(0)
  const blocked = filesUnavailableReason({ status, lifecycle })

  const form = useForm<PathFormValues>({
    resolver: zodResolver(pathFormSchema),
    defaultValues: { path: defaultFilePath(workDir) },
  })

  const statQuery = useQuery({
    ...statPathOptions({
      path: { id: sandboxId },
      query: { path: openPath ?? '' },
    }),
    enabled: openPath !== null && blocked === null,
    retry: false,
  })
  const stat = statQuery.data
  const readable = isEditableFile(stat)
  const readQuery = useQuery({
    ...readFileOptions({
      path: { id: sandboxId },
      query: { path: openPath ?? '' },
    }),
    enabled: openPath !== null && readable,
    retry: false,
    // The editor owns the text once loaded, so nothing refetches in the
    // background; "Open file" reads it again on purpose (see `openFile`).
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    staleTime: 0,
    gcTime: 0,
  })

  // `fs/write` rewrites a file at 0644 unless a mode is sent, and `fs/stat`
  // does not report one, so read it (`stat -c %a`) and send it back with the
  // save. Without this, editing a script drops its executable bit and a 0600
  // file becomes world-readable. `null` means it could not be read.
  const propertiesQuery = useQuery({
    queryKey: filePropertiesQueryKey(sandboxId, openPath ?? ''),
    queryFn: async (): Promise<FileProperties | null> => {
      const { data } = await exec({
        path: { id: sandboxId },
        body: { cmd: filePropertiesArgv(openPath ?? '') },
        throwOnError: true,
      })
      return data.exit_code === 0 ? parseFileProperties(data.stdout) : null
    },
    enabled:
      openPath !== null && blocked === null && !!stat?.exists && readable,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    staleTime: 0,
    gcTime: 0,
  })

  // Opening the same path again must show what is in the sandbox now: the
  // terminal beside this card can change the file, and saving a stale buffer
  // would silently overwrite that change.
  //
  // `refetch()` ignores `enabled`, so the read is only forced when the file
  // was already loadable AND still is; a missing, oversized or directory path
  // never gets an `fs/read`. A file that has appeared since is picked up by
  // the read query enabling itself once the fresh stat says it is readable.
  const openFile = async (path: string) => {
    if (path !== openPath) {
      setOpenPath(path)
      return
    }
    const wasReadable = readable
    const fresh = await statQuery.refetch()
    if (wasReadable && isEditableFile(fresh.data)) {
      await Promise.all([readQuery.refetch(), propertiesQuery.refetch()])
    }
    setGeneration((current) => current + 1)
  }

  // What the file looks like right now, read when Save is pressed. The cached
  // properties are from when the file was opened; the terminal beside this card
  // can have replaced it with a symlink or changed its owner since. A probe
  // that fails reads as "unknown", which asks before saving.
  const readFreshProperties = async (): Promise<FileProperties | null> => {
    const result = await propertiesQuery.refetch()
    return result.isSuccess ? result.data : null
  }

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <FileText className="h-4 w-4" />
          Files
        </CardTitle>
        <CardDescription>
          Open a text file by its absolute path, edit it and save it back into
          the sandbox; its permissions are kept, and you are asked first when a
          save would change ownership or a symbolic link. Opening a file again
          reloads it and discards unsaved edits. Browsing directories needs a
          listing endpoint that does not exist yet.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {blocked && (
          <Alert>
            <AlertTitle>Files unavailable</AlertTitle>
            <AlertDescription>{blocked}</AlertDescription>
          </Alert>
        )}
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit((values) =>
              openFile(values.path.trim())
            )}
            className="flex flex-col gap-2 sm:flex-row sm:items-end"
            noValidate
          >
            <FormField
              control={form.control}
              name="path"
              render={({ field }) => (
                <FormItem className="min-w-0 flex-1">
                  <FormLabel>File path</FormLabel>
                  <FormControl>
                    <Input
                      className="font-mono"
                      autoComplete="off"
                      spellCheck={false}
                      disabled={blocked !== null}
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button
              type="submit"
              variant="outline"
              disabled={blocked !== null || statQuery.isFetching}
            >
              Open file
            </Button>
          </form>
        </Form>

        {/* Always mounted: when the sandbox stops, the editor must keep the
            operator's unsaved text (read-only) instead of being torn down and
            rebuilt from the last saved contents on resume. */}
        <FileSection
          sandboxId={sandboxId}
          path={openPath}
          locked={blocked !== null}
          statError={statQuery.isError ? statQuery.error : null}
          statLoading={statQuery.isPending}
          stat={stat}
          readLoading={
            readable && (readQuery.isPending || propertiesQuery.isPending)
          }
          properties={propertiesQuery.isSuccess ? propertiesQuery.data : null}
          readFreshProperties={readFreshProperties}
          readError={readQuery.isError ? readQuery.error : null}
          contentsB64={readQuery.data?.contents_b64}
          generation={generation}
        />
      </CardContent>
    </Card>
  )
}

interface FileSectionProps {
  sandboxId: string
  /** The path being edited; `null` before anything has been opened. */
  path: string | null
  /** The sandbox cannot serve files right now: show what is loaded, read-only. */
  locked: boolean
  stat:
    | { exists: boolean; is_file: boolean; is_dir: boolean; size: number }
    | undefined
  statLoading: boolean
  statError: unknown
  readLoading: boolean
  readError: unknown
  contentsB64: string | undefined
  /** Mode and owner of an existing file; `null` when they could not be read. */
  properties: FileProperties | null
  readFreshProperties: () => Promise<FileProperties | null>
  generation: number
}

function FileSection({
  sandboxId,
  path,
  locked,
  stat,
  statLoading,
  statError,
  readLoading,
  readError,
  contentsB64,
  properties,
  readFreshProperties,
  generation,
}: FileSectionProps) {
  if (path === null) return null
  if (statLoading || readLoading) {
    // Locked and never loaded: the alert above already says why.
    return locked ? null : <Skeleton className="h-40 w-full" />
  }
  if (statError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>
          Could not look up {path}:{' '}
          {problemDetail(statError, 'request failed.')}
        </AlertDescription>
      </Alert>
    )
  }
  if (!stat) return null
  if (stat.is_dir) {
    return (
      <Alert>
        <AlertDescription>
          {path} is a directory. Enter the path of a file inside it.
        </AlertDescription>
      </Alert>
    )
  }
  if (stat.exists && !stat.is_file) {
    return (
      <Alert>
        <AlertDescription>
          {path} is not a regular file, so it cannot be edited here.
        </AlertDescription>
      </Alert>
    )
  }
  if (stat.exists && stat.size > MAX_EDITABLE_BYTES) {
    return (
      <Alert>
        <AlertDescription>
          {path} is {Math.round(stat.size / 1024)} KiB, over the{' '}
          {MAX_EDITABLE_BYTES / 1024} KiB the console edits. Use the terminal or
          the CLI for larger files.
        </AlertDescription>
      </Alert>
    )
  }
  if (readError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>
          Could not read {path}: {problemDetail(readError, 'request failed.')}
        </AlertDescription>
      </Alert>
    )
  }

  let initial = ''
  if (stat.exists) {
    const decoded = decodeFile(contentsB64 ?? '')
    if (!decoded.ok) {
      return (
        <Alert>
          <AlertDescription>
            {path} is not UTF-8 text, so it cannot be edited here.
          </AlertDescription>
        </Alert>
      )
    }
    initial = decoded.text
  }

  return (
    <FileEditor
      key={`${path}:${generation}`}
      sandboxId={sandboxId}
      path={path}
      exists={stat.exists}
      initial={initial}
      properties={properties}
      readFreshProperties={readFreshProperties}
      locked={locked}
    />
  )
}

function FileEditor({
  sandboxId,
  path,
  exists,
  initial,
  properties,
  readFreshProperties,
  locked,
}: {
  sandboxId: string
  path: string
  exists: boolean
  initial: string
  /** What an existing file looked like; `null` when it could not be read. */
  properties: FileProperties | null
  /** Probes the file again, at the moment of saving. */
  readFreshProperties: () => Promise<FileProperties | null>
  locked: boolean
}) {
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState(initial)
  // What the sandbox holds as far as this editor knows. A save moves it to
  // the text that was *sent*, so anything typed while the request was in
  // flight stays in the editor, still marked unsaved.
  const [saved, setSaved] = useState(initial)
  const dirty = draft !== saved
  // What a save would change besides the text, as found by the probe that
  // runs when Save is pressed; set only when the operator has to confirm.
  const [review, setReview] = useState<{
    risks: string[]
    mode: number
  } | null>(null)

  const save = useMutation({
    ...writeFileMutation(),
    meta: { errorTitle: 'Failed to save file' },
    onSuccess: (_data, variables) => {
      toast.success(`Saved ${path}`)
      // Seed the read cache with what was written instead of invalidating
      // it: a refetch would restart the editor under the user's cursor, and a
      // new file's read must not show a loading state that unmounts it.
      queryClient.setQueryData(
        readFileQueryKey({ path: { id: sandboxId }, query: { path } }),
        {
          path,
          size: atob(variables.body.contents_b64).length,
          contents_b64: variables.body.contents_b64,
        }
      )
      // Same for the file's properties, so a new file does not flash a
      // loading state and the next save compares against what is there now.
      queryClient.setQueryData(
        filePropertiesQueryKey(sandboxId, path),
        propertiesAfterSave(path, variables.body.mode ?? DEFAULT_FILE_MODE)
      )
      void queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as { _id?: string } | undefined)?._id ===
          'statPath',
      })
    },
  })

  const send = (mode: number) => {
    const sent = draft
    save.mutate(
      {
        path: { id: sandboxId },
        body: {
          path,
          contents_b64: encodeText(sent),
          // Always explicit: omitting it makes the API write 0644.
          mode,
        },
      },
      { onSuccess: () => setSaved(sent) }
    )
  }

  // Decide from the file as it is now, not as it was when it was opened: if it
  // has since become a symlink or changed owner, the operator is asked. The
  // editor text is untouched by the probe. A file that did not exist when it
  // was opened has nothing to compare against and is written at 0644.
  const preflight = useMutation({
    mutationFn: readFreshProperties,
    meta: { errorTitle: 'Failed to check the file before saving' },
    onSuccess: (fresh) => {
      const risks = saveRisks(path, fresh)
      const mode = writeMode(fresh)
      if (risks.length > 0) setReview({ risks, mode })
      else send(mode)
    },
  })

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate font-mono text-xs">{path}</span>
          {!exists && <Badge variant="neutral-subtle">new file</Badge>}
          {exists && properties && (
            <Badge variant="outline" className="font-mono">
              {formatFileMode(properties.mode & 0o777)}
            </Badge>
          )}
          {dirty && <Badge variant="warning-subtle">unsaved changes</Badge>}
        </div>
        <Button
          size="sm"
          disabled={!dirty || save.isPending || preflight.isPending || locked}
          onClick={() => {
            // Anything a save would change besides the text is said first,
            // not done silently.
            if (exists) preflight.mutate()
            else send(DEFAULT_FILE_MODE)
          }}
        >
          {save.isPending || preflight.isPending ? (
            <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
          ) : (
            <Save className="mr-1.5 h-4 w-4" />
          )}
          Save
        </Button>
      </div>
      <Textarea
        aria-label={`Contents of ${path}`}
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        readOnly={locked}
        spellCheck={false}
        className="min-h-[240px] font-mono text-xs"
      />
      <ConfirmDialog
        open={review !== null}
        onOpenChange={(open) => {
          if (!open) setReview(null)
        }}
        title="Saving will change more than the text"
        description={
          <>
            Saving <span className="font-mono">{path}</span> from here:
            <span className="mt-2 block">
              {(review?.risks ?? []).map((risk) => (
                <span key={risk} className="mb-1 block">
                  • {risk}
                </span>
              ))}
            </span>
            <span className="mt-2 block">
              Edit it from the terminal if that is not what you want.
            </span>
          </>
        }
        confirmLabel="Save anyway"
        destructive={false}
        onConfirm={() => {
          if (review) send(review.mode)
          setReview(null)
        }}
      />
    </div>
  )
}
