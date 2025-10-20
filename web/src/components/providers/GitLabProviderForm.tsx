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
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Separator } from '@/components/ui/separator'
import { createGitProviderMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation } from '@tanstack/react-query'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'
import { GitBranch, ExternalLink, Key } from 'lucide-react'

const formSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  base_url: z.string().min(1, 'GitLab URL is required'),
  token: z.string().min(1, 'Personal Access Token is required'),
})

type FormData = z.infer<typeof formSchema>

interface GitLabProviderFormProps {
  onSuccess?: () => void
}

export function GitLabProviderForm({ onSuccess }: GitLabProviderFormProps) {
  const createProviderMutation = useMutation({
    ...createGitProviderMutation(),
    meta: {
      errorTitle: 'Failed to add GitLab provider',
    },
    onSuccess: () => {
      toast.success('GitLab provider added successfully')
      onSuccess?.()
    },
  })

  const form = useForm<FormData>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: '',
      base_url: 'https://gitlab.com',
      token: '',
    },
  })

  const onSubmit = (data: FormData) => {
    const payload = {
      body: {
        name: data.name,
        provider_type: 'gitlab' as const,
        base_url: data.base_url,
        auth_method: 'pat' as const,
        auth_config: {
          token: data.token,
        },
      },
    }
    createProviderMutation.mutate(payload)
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <GitBranch className="h-5 w-5" />
            GitLab Configuration
          </CardTitle>
          <CardDescription>
            Connect to GitLab.com or your self-hosted GitLab instance
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="rounded-lg bg-muted/50 p-4 mb-6">
            <h4 className="font-medium mb-2 flex items-center gap-2">
              <Key className="h-4 w-4" />
              Personal Access Token Authentication
            </h4>
            <p className="text-sm text-muted-foreground mb-3">
              Simple and secure authentication using GitLab Personal Access
              Tokens.
            </p>
            <div className="space-y-2 text-sm">
              <div className="flex items-center gap-2">
                <div className="h-1.5 w-1.5 bg-orange-500 rounded-full" />
                <span>Works with GitLab.com and self-hosted</span>
              </div>
              <div className="flex items-center gap-2">
                <div className="h-1.5 w-1.5 bg-orange-500 rounded-full" />
                <span>Fine-grained project access</span>
              </div>
              <div className="flex items-center gap-2">
                <div className="h-1.5 w-1.5 bg-orange-500 rounded-full" />
                <span>No callback URL required</span>
              </div>
            </div>
          </div>

          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Provider Name</FormLabel>
                    <FormControl>
                      <Input placeholder="My GitLab Account" {...field} />
                    </FormControl>
                    <FormDescription>
                      A friendly name to identify this GitLab connection
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="base_url"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>GitLab URL</FormLabel>
                    <FormControl>
                      <Input placeholder="https://gitlab.com" {...field} />
                    </FormControl>
                    <FormDescription>
                      Use https://gitlab.com for GitLab.com or your self-hosted
                      GitLab URL
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="token"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Personal Access Token</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        placeholder="glpat-..."
                        {...field}
                      />
                    </FormControl>
                    <FormDescription className="space-y-2">
                      <div>Create a token with these scopes:</div>
                      <div className="text-xs bg-muted p-2 rounded font-mono">
                        <div>• api (full API access)</div>
                        <div>• read_repository (read repositories)</div>
                        <div>• write_repository (write repositories)</div>
                      </div>
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <div className="flex items-center gap-2 pt-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    const baseUrl =
                      form.getValues('base_url') || 'https://gitlab.com'
                    const tokenUrl = `${baseUrl}/-/profile/personal_access_tokens`
                    window.open(tokenUrl, '_blank')
                  }}
                >
                  <ExternalLink className="h-4 w-4 mr-2" />
                  Create Token
                </Button>
                <span className="text-sm text-muted-foreground">
                  Opens GitLab in a new tab
                </span>
              </div>

              <div className="rounded-lg bg-orange-50 dark:bg-orange-950/20 p-3 text-sm">
                <div className="font-medium text-orange-900 dark:text-orange-100 mb-2">
                  💡 Token Configuration Tips
                </div>
                <div className="text-orange-700 dark:text-orange-300 space-y-1">
                  <div>• Set expiration date (recommended: 1 year)</div>
                  <div>• Use a descriptive name like "Temps Deployment"</div>
                  <div>• Keep your token secure and never share it</div>
                </div>
              </div>

              <Separator />

              <Button
                type="submit"
                disabled={createProviderMutation.isPending}
                className="w-full"
              >
                {createProviderMutation.isPending
                  ? 'Adding Provider...'
                  : 'Add GitLab Provider'}
              </Button>
            </form>
          </Form>
        </CardContent>
      </Card>
    </div>
  )
}
