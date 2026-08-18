import { ProjectResponse } from '@/api/client'
import { updateProjectSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  // Empty string means "use the system default" and is sent as an explicit
  // null, which clears the per-project override.
  imageRetentionHours: z.string().refine(
    (value) => {
      if (value.trim() === '') return true
      const parsed = Number(value)
      return Number.isInteger(parsed) && parsed >= 1 && parsed <= 8760
    },
    { message: 'Must be a whole number of hours between 1 and 8760, or blank' }
  ),
})

type FormValues = z.infer<typeof schema>

/**
 * How long built images survive, which is in practice the rollback window —
 * a rollback needs the target deployment's image to still exist.
 */
export function ImageRetentionCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: { errorTitle: 'Failed to update image retention' },
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      imageRetentionHours:
        project?.image_retention_hours != null
          ? String(project.image_retention_hours)
          : '',
    },
  })

  const retentionInput = form.watch('imageRetentionHours')
  const retentionHours =
    retentionInput.trim() === '' ? null : Number(retentionInput)

  const handleSave = async (values: FormValues) => {
    if (!project?.id) return
    const trimmed = values.imageRetentionHours.trim()
    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id! },
        body: {
          // Explicit null clears the override back to the system default.
          image_retention_hours: trimmed === '' ? null : parseInt(trimmed, 10),
        },
      }),
      {
        loading: 'Updating image retention...',
        success: 'Image retention updated successfully',
        error: 'Failed to update image retention',
      }
    )
    refetch()
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSave)}>
        <Card className="bg-background text-foreground">
          <CardHeader>
            <CardTitle>Built Image Retention</CardTitle>
            <CardDescription>
              How long this project&apos;s built Docker images are kept before
              the nightly cleanup removes them. Rolling back or promoting a
              deployment requires its image, so this is effectively the
              project&apos;s rollback window.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <FormField
              control={form.control}
              name="imageRetentionHours"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Retention (hours)</FormLabel>
                  <FormControl>
                    <Input
                      type="number"
                      min={1}
                      max={8760}
                      placeholder="Use system default"
                      className="max-w-xs"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    Leave blank to use the system-wide default configured in
                    Settings. Min 1 hour, max 8760 (one year).
                  </FormDescription>
                </FormItem>
              )}
            />
            {retentionHours !== null && retentionHours < 48 && (
              <p className="mt-3 text-sm text-destructive">
                At {retentionHours}h, deployments older than that can no longer
                be rolled back to — their images will already have been deleted.
              </p>
            )}
          </CardContent>
          <CardFooter>
            <Button type="submit" disabled={updateProjectSettings.isPending}>
              Save Retention
            </Button>
          </CardFooter>
        </Card>
      </form>
    </Form>
  )
}
