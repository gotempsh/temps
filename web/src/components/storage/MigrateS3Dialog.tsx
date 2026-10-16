// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getServiceQueryKey,
  listServicesOptions,
  migrateS3ServiceMutation,
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
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const S3_SOURCE_TYPES = ['s3', 'blob', 'rustfs']

const migrateFormSchema = z.object({
  source_service_id: z.string().min(1, 'Choose the service to copy from'),
})

type MigrateFormValues = z.infer<typeof migrateFormSchema>

interface MigrateS3DialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** The service that receives the data. */
  serviceId: number
  serviceName: string
  onSuccess?: () => void
}

/**
 * Copies every bucket of another S3 service into this one. Switching engines
 * is "create the new service, migrate, then repoint" — this is the middle step.
 */
export function MigrateS3Dialog({
  open,
  onOpenChange,
  serviceId,
  serviceName,
  onSuccess,
}: MigrateS3DialogProps) {
  const queryClient = useQueryClient()
  const form = useForm<MigrateFormValues>({
    resolver: zodResolver(migrateFormSchema),
    defaultValues: { source_service_id: '' },
  })

  const { data: services, isPending: isLoadingServices } = useQuery({
    ...listServicesOptions({ query: { page_size: 100 } }),
    enabled: open,
  })
  const sources = (services ?? []).filter(
    (candidate) =>
      candidate.id !== serviceId &&
      S3_SOURCE_TYPES.includes(candidate.service_type) &&
      candidate.node_id == null
  )

  const migrate = useMutation({
    ...migrateS3ServiceMutation(),
    onSuccess: (report) => {
      toast.success(
        `Copied ${report.buckets.length} bucket(s) to ${serviceName}`,
        {
          description: `From ${report.source_backend} to ${report.target_backend}. Re-run it to pick up later writes.`,
        }
      )
      queryClient.invalidateQueries({
        queryKey: getServiceQueryKey({ path: { id: serviceId } }),
      })
      onSuccess?.()
      onOpenChange(false)
      form.reset()
    },
    onError: (error) => {
      toast.error('Failed to migrate data', {
        description: problemDetail(error, 'The copy did not complete.'),
      })
    },
  })

  const onSubmit = (values: MigrateFormValues) => {
    migrate.mutate({
      path: { id: serviceId },
      body: { source_service_id: Number(values.source_service_id) },
    })
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next)
        if (!next) form.reset()
      }}
    >
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>Migrate data into {serviceName}</DialogTitle>
          <DialogDescription>
            Copy every bucket from another S3 service (RustFS or Garage). The
            source is only read and nothing in {serviceName} is deleted.
          </DialogDescription>
        </DialogHeader>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
            <FormField
              control={form.control}
              name="source_service_id"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Copy from</FormLabel>
                  {isLoadingServices ? (
                    <Skeleton className="h-10 w-full" />
                  ) : (
                    <Select
                      value={field.value}
                      onValueChange={field.onChange}
                      disabled={sources.length === 0}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue
                            placeholder={
                              sources.length === 0
                                ? 'No other S3 service on the control plane'
                                : 'Select a service'
                            }
                          />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {sources.map((source) => (
                          <SelectItem key={source.id} value={String(source.id)}>
                            {source.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}
                  <FormDescription>
                    Both services must run on the control plane. Create the
                    target service first to switch engines.
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
              <Button type="submit" disabled={migrate.isPending}>
                {migrate.isPending && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Copy data
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
