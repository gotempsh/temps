import { upgradeServiceMutation } from '@/api/client/@tanstack/react-query.gen'
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
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const CUSTOM_IMAGE_VALUE = '__custom__'

const upgradeFormSchema = z.object({
  docker_image: z
    .string()
    .min(1, 'Docker image is required')
    .regex(
      /^[\w.\-/:]+$/,
      'Invalid Docker image format. Example: postgres:18-bookworm'
    ),
})

type UpgradeFormValues = z.infer<typeof upgradeFormSchema>

export interface SupportedImage {
  image: string
  label: string
}

/**
 * Returns the list of supported WAL-G images for a given service type.
 */
export function getSupportedImages(serviceType: string): SupportedImage[] {
  if (serviceType === 'postgres') {
    return [
      { image: 'gotempsh/postgres-walg:18-bookworm', label: 'PostgreSQL 18 + WAL-G' },
      { image: 'gotempsh/postgres-walg:17-bookworm', label: 'PostgreSQL 17 + WAL-G' },
      { image: 'gotempsh/pgvector-walg:pg18', label: 'pgvector 18 + WAL-G' },
      { image: 'gotempsh/pgvector-walg:pg17', label: 'pgvector 17 + WAL-G' },
      { image: 'gotempsh/timescaledb-walg:pg18', label: 'TimescaleDB (PG 18) + WAL-G' },
    ]
  }
  if (serviceType === 'redis') {
    return [
      { image: 'gotempsh/redis-walg:8-bookworm', label: 'Redis 8 + WAL-G' },
    ]
  }
  if (serviceType === 'mongodb') {
    return [
      { image: 'gotempsh/mongodb-walg:8.0', label: 'MongoDB 8.0 + WAL-G' },
    ]
  }
  return []
}

interface UpgradeServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  serviceName: string
  currentImage?: string
  serviceType: string
  /** Called after a successful upgrade so the parent can refetch service data. */
  onSuccess?: () => void
}

export function UpgradeServiceDialog({
  open,
  onOpenChange,
  serviceId,
  serviceName,
  currentImage,
  serviceType,
  onSuccess,
}: UpgradeServiceDialogProps) {
  const queryClient = useQueryClient()
  const supportedImages = getSupportedImages(serviceType)

  const form = useForm<UpgradeFormValues>({
    resolver: zodResolver(upgradeFormSchema),
    defaultValues: {
      docker_image: '',
    },
  })

  const selectedImage = form.watch('docker_image')

  // Track whether the user picked "Custom" from the select
  const isCustom =
    selectedImage !== '' &&
    !supportedImages.some((s) => s.image === selectedImage)

  const upgradeService = useMutation({
    ...upgradeServiceMutation(),
    onSuccess: () => {
      toast.success(
        `${serviceName} is being upgraded. This may take a few minutes.`
      )
      queryClient.invalidateQueries({
        queryKey: ['get', '/external-services/:id'],
      })
      // Let the parent refetch the service detail (image, status) with its
      // own query key — the invalidate above doesn't match getServiceOptions.
      onSuccess?.()
      onOpenChange(false)
      form.reset()
    },
    onError: (error: Error) => {
      toast.error('Failed to upgrade service', {
        description:
          (error as any).detail || error.message || 'An unexpected error occurred',
      })
    },
  })

  const onSubmit = (values: UpgradeFormValues) => {
    upgradeService.mutate({
      path: { id: serviceId },
      body: {
        docker_image: values.docker_image,
      },
    })
  }

  const handleCancel = () => {
    form.reset()
    onOpenChange(false)
  }

  // When select changes, either set the image directly or clear for custom input
  const handleSelectChange = (value: string) => {
    if (value === CUSTOM_IMAGE_VALUE) {
      form.setValue('docker_image', '', { shouldValidate: false })
    } else {
      form.setValue('docker_image', value, { shouldValidate: true })
    }
  }

  // Determine select value: if it matches a supported image show that,
  // if user typed something custom show the custom sentinel, otherwise empty
  const selectValue = supportedImages.some((s) => s.image === selectedImage)
    ? selectedImage
    : isCustom || selectedImage === ''
      ? CUSTOM_IMAGE_VALUE
      : ''

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        onOpenChange(o)
        if (!o) form.reset()
      }}
    >
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>Upgrade {serviceName}</DialogTitle>
          <DialogDescription>
            Select a new Docker image. The service will be stopped during the
            upgrade. Data is preserved on the Docker volume.
          </DialogDescription>
        </DialogHeader>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
            {currentImage && (
              <div className="rounded-lg border bg-muted/50 p-3">
                <p className="text-sm font-medium mb-1">Current Image</p>
                <code className="text-xs text-muted-foreground break-all">
                  {currentImage}
                </code>
              </div>
            )}

            <FormField
              control={form.control}
              name="docker_image"
              render={() => (
                <FormItem>
                  <FormLabel>New Docker Image</FormLabel>
                  <Select
                    value={selectValue}
                    onValueChange={handleSelectChange}
                    disabled={upgradeService.isPending}
                  >
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue placeholder="Select an image..." />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      {supportedImages.map((img) => (
                        <SelectItem
                          key={img.image}
                          value={img.image}
                          disabled={currentImage === img.image}
                        >
                          {img.label}
                          {currentImage === img.image ? ' (current)' : ''}
                        </SelectItem>
                      ))}
                      <SelectItem value={CUSTOM_IMAGE_VALUE}>
                        Custom image...
                      </SelectItem>
                    </SelectContent>
                  </Select>
                  <FormDescription>
                    WAL-G images include streaming backups to S3.{' '}
                    {serviceType === 'postgres' &&
                      'PostgreSQL images also get continuous WAL archiving for point-in-time recovery.'}
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            {selectValue === CUSTOM_IMAGE_VALUE && (
              <FormField
                control={form.control}
                name="docker_image"
                render={({ field }) => (
                  <FormItem>
                    <FormControl>
                      <Input
                        placeholder="e.g. postgres:18-bookworm"
                        {...field}
                        disabled={upgradeService.isPending}
                        autoFocus
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            {!currentImage?.includes('gotempsh/') &&
              selectedImage?.includes('gotempsh/') && (
                <div className="rounded-lg border border-blue-500/20 bg-blue-500/10 p-3 flex gap-2">
                  <AlertCircle className="h-4 w-4 text-blue-600 dark:text-blue-400 mt-0.5 flex-shrink-0" />
                  <p className="text-xs text-blue-700 dark:text-blue-300">
                    WAL-G images provide streaming backups directly to S3
                    with constant memory usage. PostgreSQL images also get
                    continuous WAL archiving for point-in-time recovery.
                  </p>
                </div>
              )}

            <div className="rounded-lg border border-yellow-500/20 bg-yellow-500/10 p-3 flex gap-2">
              <AlertCircle className="h-4 w-4 text-yellow-600 dark:text-yellow-500 mt-0.5 flex-shrink-0" />
              <div className="space-y-1">
                <p className="text-sm font-medium text-yellow-800 dark:text-yellow-200">
                  Important
                </p>
                <p className="text-xs text-yellow-700 dark:text-yellow-300">
                  The service will be stopped during the upgrade. For
                  PostgreSQL major version changes (e.g., 17 → 18), data
                  migration runs automatically via pg_upgrade.
                </p>
              </div>
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={handleCancel}
                disabled={upgradeService.isPending}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={upgradeService.isPending || !selectedImage}
              >
                {upgradeService.isPending && (
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                )}
                Upgrade Service
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
