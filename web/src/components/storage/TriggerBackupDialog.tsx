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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertCircle, HardDrive, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Link } from 'react-router-dom'

const formSchema = z.object({
  s3_source_id: z.coerce.number({ required_error: 'Please select an S3 source' }),
  backup_type: z.string().optional(),
})

type FormValues = z.infer<typeof formSchema>

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
    resolver: zodResolver(formSchema),
    defaultValues: {
      backup_type: 'full',
    },
  })

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
    runBackupMutation.mutate({
      path: { id: serviceId },
      body: {
        s3_source_id: values.s3_source_id,
        backup_type: values.backup_type || 'full',
      },
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
                      onValueChange={field.onChange}
                      value={field.value?.toString()}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select an S3 source" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {s3Sources?.map((source) => (
                          <SelectItem
                            key={source.id}
                            value={source.id.toString()}
                          >
                            <div className="flex flex-col">
                              <span>{source.name}</span>
                              <span className="text-xs text-muted-foreground">
                                {source.bucket_name}
                                {source.bucket_path && `/${source.bucket_path}`}
                              </span>
                            </div>
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      The S3-compatible storage where the backup will be saved
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
