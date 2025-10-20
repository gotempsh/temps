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
import { zodResolver } from '@hookform/resolvers/zod'
import { Plus } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'

const formSchema = z.object({
  name: z.string().min(1, 'Environment name is required').max(50),
  branch: z.string().min(1, 'Branch name is required'),
})

type FormValues = z.infer<typeof formSchema>

interface CreateEnvironmentDialogProps {
  onSubmit: (values: FormValues) => Promise<void>
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function CreateEnvironmentDialog({
  onSubmit,
  open,
  onOpenChange,
}: CreateEnvironmentDialogProps) {
  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: '',
      branch: 'main',
    },
  })

  const handleSubmit = async (values: FormValues) => {
    try {
      await onSubmit(values)
      form.reset()
      onOpenChange(false)
    } catch (error) {
      // Error handling will be done by the parent component
      throw error
    }
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
                    <Input
                      placeholder="e.g., main, develop, feature/branch"
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
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
