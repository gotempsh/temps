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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { createGitProviderMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation } from '@tanstack/react-query'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'
import { GithubIcon, ExternalLink, Key, Shield } from 'lucide-react'
import { useState } from 'react'

const patFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  base_url: z.string().url('Invalid URL format').optional().or(z.literal('')),
  token: z.string().min(1, 'Personal Access Token is required'),
})

const oauthFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  base_url: z.string().url('Invalid URL format').optional().or(z.literal('')),
  client_id: z.string().min(1, 'Client ID is required'),
  client_secret: z.string().min(1, 'Client Secret is required'),
})

type PATFormData = z.infer<typeof patFormSchema>
type OAuthFormData = z.infer<typeof oauthFormSchema>

interface GitHubProviderFormProps {
  onSuccess?: () => void
}

export function GitHubProviderForm({ onSuccess }: GitHubProviderFormProps) {
  const [authMethod, setAuthMethod] = useState<'pat' | 'oauth'>('pat')

  const createProviderMutation = useMutation({
    ...createGitProviderMutation(),
    meta: {
      errorTitle: 'Failed to add GitHub provider',
    },
    onSuccess: () => {
      toast.success('GitHub provider added successfully')
      onSuccess?.()
    },
  })

  const patForm = useForm<PATFormData>({
    resolver: zodResolver(patFormSchema),
    defaultValues: {
      name: '',
      base_url: '',
      token: '',
    },
  })

  const oauthForm = useForm<OAuthFormData>({
    resolver: zodResolver(oauthFormSchema),
    defaultValues: {
      name: '',
      base_url: '',
      client_id: '',
      client_secret: '',
    },
  })

  const onSubmitPAT = (data: PATFormData) => {
    const payload = {
      body: {
        name: data.name,
        provider_type: 'github' as const,
        base_url: data.base_url || null,
        auth_method: 'pat' as const,
        auth_config: {
          token: data.token,
        },
      },
    }
    createProviderMutation.mutate(payload)
  }

  const onSubmitOAuth = (data: OAuthFormData) => {
    const payload = {
      body: {
        name: data.name,
        provider_type: 'github' as const,
        base_url: data.base_url || null,
        auth_method: 'oauth' as const,
        auth_config: {
          client_id: data.client_id,
          client_secret: data.client_secret,
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
            <GithubIcon className="h-5 w-5" />
            GitHub Configuration
          </CardTitle>
          <CardDescription>
            Choose how you want to authenticate with GitHub
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Tabs
            value={authMethod}
            onValueChange={(value) => setAuthMethod(value as 'pat' | 'oauth')}
          >
            <TabsList className="grid w-full grid-cols-2">
              <TabsTrigger value="pat" className="flex items-center gap-2">
                <Key className="h-4 w-4" />
                Personal Access Token
              </TabsTrigger>
              <TabsTrigger value="oauth" className="flex items-center gap-2">
                <Shield className="h-4 w-4" />
                OAuth App
              </TabsTrigger>
            </TabsList>

            <TabsContent value="pat" className="space-y-4">
              <div className="rounded-lg bg-muted/50 p-4">
                <h4 className="font-medium mb-2">
                  Personal Access Token (Recommended)
                </h4>
                <p className="text-sm text-muted-foreground mb-3">
                  Simple setup with fine-grained permissions. Perfect for
                  individual developers or small teams.
                </p>
                <div className="space-y-2 text-sm">
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 w-1.5 bg-green-500 rounded-full" />
                    <span>Quick setup (2 minutes)</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 w-1.5 bg-green-500 rounded-full" />
                    <span>Fine-grained repository access</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 w-1.5 bg-green-500 rounded-full" />
                    <span>No callback URL required</span>
                  </div>
                </div>
              </div>

              <Form {...patForm}>
                <form
                  onSubmit={patForm.handleSubmit(onSubmitPAT)}
                  className="space-y-4"
                >
                  <FormField
                    control={patForm.control}
                    name="name"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Provider Name</FormLabel>
                        <FormControl>
                          <Input placeholder="My GitHub Account" {...field} />
                        </FormControl>
                        <FormDescription>
                          A friendly name to identify this GitHub connection
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={patForm.control}
                    name="base_url"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Base URL (Optional)</FormLabel>
                        <FormControl>
                          <Input placeholder="https://github.com" {...field} />
                        </FormControl>
                        <FormDescription>
                          Leave empty for GitHub.com or enter your GitHub
                          Enterprise URL
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={patForm.control}
                    name="token"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Personal Access Token</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="ghp_..."
                            {...field}
                          />
                        </FormControl>
                        <FormDescription className="space-y-2">
                          <div>Create a token with these permissions:</div>
                          <div className="text-xs bg-muted p-2 rounded font-mono">
                            <div>• Contents (read)</div>
                            <div>• Metadata (read)</div>
                            <div>• Pull requests (read & write)</div>
                            <div>• Webhooks (write) - optional</div>
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
                      onClick={() =>
                        window.open(
                          'https://github.com/settings/tokens/new',
                          '_blank'
                        )
                      }
                    >
                      <ExternalLink className="h-4 w-4 mr-2" />
                      Create Token
                    </Button>
                    <span className="text-sm text-muted-foreground">
                      Opens GitHub in a new tab
                    </span>
                  </div>

                  <Separator />

                  <Button
                    type="submit"
                    disabled={createProviderMutation.isPending}
                    className="w-full"
                  >
                    {createProviderMutation.isPending
                      ? 'Adding Provider...'
                      : 'Add GitHub Provider'}
                  </Button>
                </form>
              </Form>
            </TabsContent>

            <TabsContent value="oauth" className="space-y-4">
              <div className="rounded-lg bg-muted/50 p-4">
                <h4 className="font-medium mb-2">OAuth Application</h4>
                <p className="text-sm text-muted-foreground mb-3">
                  More secure for production environments. Requires creating a
                  GitHub OAuth App.
                </p>
                <div className="space-y-2 text-sm">
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 w-1.5 bg-blue-500 rounded-full" />
                    <span>Enhanced security</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 w-1.5 bg-blue-500 rounded-full" />
                    <span>User-based permissions</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 w-1.5 bg-blue-500 rounded-full" />
                    <span>Requires OAuth App setup</span>
                  </div>
                </div>
              </div>

              <Form {...oauthForm}>
                <form
                  onSubmit={oauthForm.handleSubmit(onSubmitOAuth)}
                  className="space-y-4"
                >
                  <FormField
                    control={oauthForm.control}
                    name="name"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Provider Name</FormLabel>
                        <FormControl>
                          <Input placeholder="My GitHub OAuth App" {...field} />
                        </FormControl>
                        <FormDescription>
                          A friendly name to identify this GitHub connection
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={oauthForm.control}
                    name="base_url"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Base URL (Optional)</FormLabel>
                        <FormControl>
                          <Input placeholder="https://github.com" {...field} />
                        </FormControl>
                        <FormDescription>
                          Leave empty for GitHub.com or enter your GitHub
                          Enterprise URL
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={oauthForm.control}
                    name="client_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Client ID</FormLabel>
                        <FormControl>
                          <Input placeholder="Ov23li..." {...field} />
                        </FormControl>
                        <FormDescription>
                          The Client ID from your GitHub OAuth App
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={oauthForm.control}
                    name="client_secret"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Client Secret</FormLabel>
                        <FormControl>
                          <Input type="password" {...field} />
                        </FormControl>
                        <FormDescription>
                          The Client Secret from your GitHub OAuth App
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
                      onClick={() =>
                        window.open(
                          'https://github.com/settings/applications/new',
                          '_blank'
                        )
                      }
                    >
                      <ExternalLink className="h-4 w-4 mr-2" />
                      Create OAuth App
                    </Button>
                    <span className="text-sm text-muted-foreground">
                      Opens GitHub in a new tab
                    </span>
                  </div>

                  <div className="rounded-lg bg-blue-50 dark:bg-blue-950/20 p-3 text-sm">
                    <div className="font-medium text-blue-900 dark:text-blue-100 mb-1">
                      OAuth App Configuration
                    </div>
                    <div className="text-blue-700 dark:text-blue-300 space-y-1">
                      <div>
                        <strong>Authorization callback URL:</strong>
                      </div>
                      <div className="font-mono text-xs bg-blue-100 dark:bg-blue-900/50 p-1 rounded">
                        {window.location.origin}/api/auth/github/callback
                      </div>
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
                      : 'Add GitHub Provider'}
                  </Button>
                </form>
              </Form>
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>
    </div>
  )
}
