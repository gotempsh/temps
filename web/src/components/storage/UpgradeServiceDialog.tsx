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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const upgradeFormSchema = z.object({
  docker_image: z
    .string()
    .min(1, 'Docker image is required')
    .regex(
      /^[\w.\-/:]+$/,
      'Invalid Docker image format. Example: postgres:17-alpine'
    ),
})

type UpgradeFormValues = z.infer<typeof upgradeFormSchema>

interface UpgradeServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  serviceName: string
  currentImage?: string
  serviceType: string
}

export function UpgradeServiceDialog({
  open,
  onOpenChange,
  serviceId,
  serviceName,
  currentImage,
  serviceType,
}: UpgradeServiceDialogProps) {
  const queryClient = useQueryClient()

  const form = useForm<UpgradeFormValues>({
    resolver: zodResolver(upgradeFormSchema),
    defaultValues: {
      docker_image: '',
    },
  })

  const upgradeService = useMutation({
    ...upgradeServiceMutation(),
    onSuccess: () => {
      toast.success(
        `${serviceName} is being upgraded. This may take a few minutes.`
      )
      queryClient.invalidateQueries({
        queryKey: ['get', '/external-services/:id'],
      })
      onOpenChange(false)
      form.reset()
    },
    onError: (error: Error) => {
      toast.error('Failed to upgrade service', {
        description: error.message || 'An unexpected error occurred',
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

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>Upgrade Service</DialogTitle>
          <DialogDescription>
            Upgrade {serviceName} to a new Docker image version. This will run
            the appropriate upgrade procedure for {serviceType}.
          </DialogDescription>
        </DialogHeader>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
            <div className="space-y-4">
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
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>New Docker Image</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="postgres:17-alpine"
                        {...field}
                        disabled={upgradeService.isPending}
                      />
                    </FormControl>
                    <FormDescription>
                      Enter the Docker image tag to upgrade to. For PostgreSQL,
                      pg_upgrade will be used for major version changes.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <div className="rounded-lg border border-yellow-500/20 bg-yellow-500/10 p-3 flex gap-2">
                <AlertCircle className="h-4 w-4 text-yellow-600 dark:text-yellow-500 mt-0.5 flex-shrink-0" />
                <div className="space-y-1">
                  <p className="text-sm font-medium text-yellow-800 dark:text-yellow-200">
                    Important
                  </p>
                  <p className="text-xs text-yellow-700 dark:text-yellow-300">
                    The service will be stopped during the upgrade process. For
                    major version upgrades (e.g., PostgreSQL 16 → 17), data
                    migration will be performed automatically.
                  </p>
                </div>
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
              <Button type="submit" disabled={upgradeService.isPending}>
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
