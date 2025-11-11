import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { zodResolver } from '@hookform/resolvers/zod'
import { Plus } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'
import { ProjectResponse } from '@/api/client'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { useState } from 'react'

const formSchema = z.object({
  name: z.string().min(1, 'Environment name is required').max(50),
  branch: z.string().min(1, 'Branch name is required'),
  isPreview: z.boolean().default(false),
})

type FormValues = z.infer<typeof formSchema>

interface CreateEnvironmentDialogProps {
  onSubmit: (values: FormValues) => Promise<void>
  open: boolean
  onOpenChange: (open: boolean) => void
  project?: ProjectResponse
}

export function CreateEnvironmentDialog({
  onSubmit,
  open,
  onOpenChange,
  project,
}: CreateEnvironmentDialogProps) {
  const [branchError, setBranchError] = useState<string | null>(null)
  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: '',
      branch: project?.main_branch || 'main',
      isPreview: false,
    },
  })

  const handleSubmit = async (values: FormValues) => {
    await onSubmit(values)
    form.reset()
    setBranchError(null)
    onOpenChange(false)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="h-4 w-4 mr-2" />
          Add Environment
        </Button>
      </DialogTrigger>
      <DialogContent>
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit(handleSubmit)}
            className="space-y-4"
          >
            <DialogHeader>
              <DialogTitle>Create Environment</DialogTitle>
              <DialogDescription>
                Add a new environment to your project for different deployment
                stages.
              </DialogDescription>
            </DialogHeader>

            <FormField
              control={form.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Environment Name</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="e.g., Production, Staging, Development"
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="branch"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Git Branch</FormLabel>
                  <FormControl>
                    {project?.repo_owner && project?.repo_name ? (
                      <BranchSelector
                        repoOwner={project.repo_owner}
                        repoName={project.repo_name}
                        connectionId={project.git_provider_connection_id || 0}
                        defaultBranch={project.main_branch}
                        value={field.value}
                        onChange={(val) => {
                          field.onChange(val)
                          setBranchError(null)
                        }}
                        onError={setBranchError}
                      />
                    ) : (
                      <Input
                        placeholder="e.g., main, develop, feature/branch"
                        {...field}
                      />
                    )}
                  </FormControl>
                  {branchError && (
                    <p className="text-sm font-medium text-destructive">
                      {branchError}
                    </p>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="isPreview"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center space-x-3 space-y-0 rounded-md border p-4">
                  <FormControl>
                    <Checkbox
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                  <div className="space-y-1 leading-none">
                    <FormLabel className="text-base font-normal cursor-pointer">
                      Preview Environment
                    </FormLabel>
                    <p className="text-sm text-muted-foreground">
                      Branches that don't match any environment will deploy here
                      (e.g., PRs, feature branches)
                    </p>
                  </div>
                </FormItem>
              )}
            />

            <DialogFooter>
              <Button type="submit" disabled={form.formState.isSubmitting}>
                {form.formState.isSubmitting
                  ? 'Creating...'
                  : 'Create Environment'}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
