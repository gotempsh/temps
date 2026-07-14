import {
  listS3SourcesOptions,
  runExternalServiceBackupMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertCircle, HardDrive, Loader2, Star } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Link } from 'react-router-dom'

type FormValues = {
  s3_source_id?: number
  backup_type?: string
}

interface TriggerBackupDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  serviceName: string
  onSuccess?: () => void
}

export function TriggerBackupDialog({
  open,
  onOpenChange,
  serviceId,
  serviceName,
  onSuccess,
}: TriggerBackupDialogProps) {
  const { data: s3Sources, isLoading: s3SourcesLoading } = useQuery({
    ...listS3SourcesOptions(),
    enabled: open,
  })

  const form = useForm<FormValues>({
    defaultValues: {
      backup_type: 'full',
    },
  })

  // The user's explicitly-flagged default S3 source, if any. Used in
  // the FormDescription render below ("Defaults to ⭐ Cloudflare …") and
  // as the first-choice preselect in the effect below.
  const defaultSource = s3Sources?.find(
    (s) => (s as { is_default?: boolean }).is_default === true
  )

  // Pre-select a Storage Destination when the dialog opens:
  //   1. The default source, if any.
  //   2. Otherwise, the first source in the list — saves an extra click
  //      and matches what most operators want when they only have one or
  //      two sources configured. Without this, the Select renders blank
  //      and the user has to expand it manually.
  useEffect(() => {
    if (!open || !s3Sources || s3Sources.length === 0) return
    if (form.getValues('s3_source_id') !== undefined) return
    const preferred = defaultSource ?? s3Sources[0]
    form.setValue('s3_source_id', preferred.id)
  }, [open, s3Sources, defaultSource, form])

  const runBackupMutation = useMutation({
    ...runExternalServiceBackupMutation(),
    meta: {
      errorTitle: 'Failed to trigger backup',
    },
    onSuccess: () => {
      toast.success('Backup started successfully', {
        description: `A backup of ${serviceName} has been triggered.`,
      })
      form.reset()
      onOpenChange(false)
      onSuccess?.()
    },
  })

  const onSubmit = (values: FormValues) => {
    const selectedSourceId =
      values.s3_source_id ?? defaultSource?.id ?? s3Sources?.[0]?.id
    // Backend accepts s3_source_id as Option<i32>; generated client type still requires number.
    const body = {
      backup_type: values.backup_type || 'full',
      ...(selectedSourceId !== undefined
        ? { s3_source_id: selectedSourceId }
        : {}),
    } as { s3_source_id: number; backup_type: string }
    runBackupMutation.mutate({
      path: { id: serviceId },
      body,
    })
  }

  const handleOpenChange = (newOpen: boolean) => {
    if (!newOpen) {
      form.reset()
    }
    onOpenChange(newOpen)
  }

  const hasS3Sources = s3Sources && s3Sources.length > 0

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <HardDrive className="h-5 w-5" />
            Trigger Backup
          </DialogTitle>
          <DialogDescription>
            Create a backup of <strong>{serviceName}</strong> and store it in an
            S3-compatible storage.
          </DialogDescription>
        </DialogHeader>

        {s3SourcesLoading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-4 w-4 animate-spin mr-2" />
            <span className="text-sm text-muted-foreground">
              Loading storage options...
            </span>
          </div>
        ) : !hasS3Sources ? (
          <div className="space-y-4">
            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                No S3 sources configured. You need to create an S3 source before
                you can trigger backups.
              </AlertDescription>
            </Alert>
            <div className="flex justify-end gap-2">
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Link to="/backups">
                <Button>Configure S3 Sources</Button>
              </Link>
            </div>
          </div>
        ) : (
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
              <FormField
                control={form.control}
                name="s3_source_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Storage Destination</FormLabel>
                    <Select
                      onValueChange={(v) => field.onChange(Number(v))}
                      value={field.value?.toString()}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select an S3 source" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {s3Sources?.map((source) => {
                          const isDefault =
                            (source as { is_default?: boolean }).is_default ===
                            true
                          return (
                            // pl-2 overrides the shadcn SelectItem default of
                            // pl-8 (which reserves a check-mark gutter we
                            // don't render here). Without this override the
                            // menu rows look indented relative to the trigger
                            // when an option is selected. The hidden check
                            // <span> inside SelectItem still positions
                            // absolutely at left-2; we accept the slight
                            // visual overlap because we never render it.
                            <SelectItem
                              key={source.id}
                              value={source.id.toString()}
                              className="pl-2"
                            >
                              <div className="flex flex-col items-start text-left">
                                <span className="flex items-center gap-1.5">
                                  {source.name}
                                  {isDefault ? (
                                    <span
                                      title="Default source"
                                      className="inline-flex items-center gap-0.5 text-xs text-amber-600 dark:text-amber-400"
                                    >
                                      <Star className="h-3 w-3 fill-current" />
                                      Default
                                    </span>
                                  ) : null}
                                </span>
                                <span className="text-xs text-muted-foreground">
                                  {source.bucket_name}
                                  {source.bucket_path &&
                                    `/${source.bucket_path}`}
                                </span>
                              </div>
                            </SelectItem>
                          )
                        })}
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      {defaultSource
                        ? `Defaults to ⭐ ${defaultSource.name}. Pick a different source to override.`
                        : 'The S3-compatible storage where the backup will be saved.'}
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="backup_type"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Backup Type</FormLabel>
                    <Select
                      onValueChange={field.onChange}
                      value={field.value || 'full'}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select backup type" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="full">Full Backup</SelectItem>
                        <SelectItem value="incremental">
                          Incremental Backup
                        </SelectItem>
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      Full backups include all data. Incremental backups only
                      include changes since the last backup.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => onOpenChange(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={runBackupMutation.isPending}>
                  {runBackupMutation.isPending && (
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  )}
                  Start Backup
                </Button>
              </DialogFooter>
            </form>
          </Form>
        )}
      </DialogContent>
    </Dialog>
  )
}
