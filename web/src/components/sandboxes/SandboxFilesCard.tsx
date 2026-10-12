// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
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
  MAX_EDITABLE_BYTES,
  decodeFile,
  defaultFilePath,
  encodeText,
  filesUnavailableReason,
  isEditableFile,
  validateSandboxPath,
} from './sandbox-files'

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
    if (wasReadable && isEditableFile(fresh.data)) void readQuery.refetch()
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
          the sandbox. Opening a file again reloads it and discards unsaved
          edits. Browsing directories needs a listing endpoint that does not
          exist yet.
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

        {openPath !== null && blocked === null && (
          <FileSection
            sandboxId={sandboxId}
            path={openPath}
            statError={statQuery.isError ? statQuery.error : null}
            statLoading={statQuery.isPending}
            stat={stat}
            readLoading={readable && readQuery.isPending}
            readError={readQuery.isError ? readQuery.error : null}
            contentsB64={readQuery.data?.contents_b64}
            loadedAt={readQuery.dataUpdatedAt}
          />
        )}
      </CardContent>
    </Card>
  )
}

interface FileSectionProps {
  sandboxId: string
  path: string
  stat:
    | { exists: boolean; is_file: boolean; is_dir: boolean; size: number }
    | undefined
  statLoading: boolean
  statError: unknown
  readLoading: boolean
  readError: unknown
  contentsB64: string | undefined
  loadedAt: number
}

function FileSection({
  sandboxId,
  path,
  stat,
  statLoading,
  statError,
  readLoading,
  readError,
  contentsB64,
  loadedAt,
}: FileSectionProps) {
  if (statLoading || readLoading) {
    return <Skeleton className="h-40 w-full" />
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
      key={`${path}:${loadedAt}:${stat.exists}`}
      sandboxId={sandboxId}
      path={path}
      exists={stat.exists}
      initial={initial}
    />
  )
}

function FileEditor({
  sandboxId,
  path,
  exists,
  initial,
}: {
  sandboxId: string
  path: string
  exists: boolean
  initial: string
}) {
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState(initial)
  const dirty = draft !== initial

  const save = useMutation({
    ...writeFileMutation(),
    meta: { errorTitle: 'Failed to save file' },
    onSuccess: () => {
      toast.success(`Saved ${path}`)
      void queryClient.invalidateQueries({
        queryKey: readFileQueryKey({
          path: { id: sandboxId },
          query: { path },
        }),
      })
      void queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as { _id?: string } | undefined)?._id ===
          'statPath',
      })
    },
  })

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate font-mono text-xs">{path}</span>
          {!exists && <Badge variant="neutral-subtle">new file</Badge>}
          {dirty && <Badge variant="warning-subtle">unsaved changes</Badge>}
        </div>
        <Button
          size="sm"
          disabled={!dirty || save.isPending}
          onClick={() =>
            save.mutate({
              path: { id: sandboxId },
              body: { path, contents_b64: encodeText(draft) },
            })
          }
        >
          {save.isPending ? (
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
        spellCheck={false}
        className="min-h-[240px] font-mono text-xs"
      />
    </div>
  )
}
