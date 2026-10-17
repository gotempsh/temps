// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { Globe, Loader2 } from 'lucide-react'
import { createProjectMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { getErrorMessage } from '@/utils/errorHandling'
import { useJoinGroupAfterCreate } from '@/components/project-groups/useJoinGroupAfterCreate'

const externalProjectSchema = z.object({
  name: z.string().trim().min(1, 'Give the service a name.'),
})

type ExternalProjectValues = z.infer<typeof externalProjectSchema>

/**
 * Create a service for an app that runs outside Temps. Temps never deploys
 * it; it receives the app's analytics, errors and traces, and labels the
 * service External. Creating it leads straight to the ingest-key setup (or,
 * from a project's "New service", to that project).
 */
export function ExternalProjectConfigurator({
  onCancel,
}: {
  onCancel: () => void
}) {
  const form = useForm<ExternalProjectValues>({
    resolver: zodResolver(externalProjectSchema),
    defaultValues: { name: '' },
  })
  const name = useWatch({ control: form.control, name: 'name' })
  const { finish, joinGroupDialog } = useJoinGroupAfterCreate()
  const create = useMutation({
    ...createProjectMutation(),
    onSuccess: (project) =>
      finish(project, `/projects/${project.slug}/analytics`),
  })

  const submit = (values: ExternalProjectValues) => {
    create.mutate({
      body: {
        name: values.name,
        hosting: 'external',
        // Required by the API, unused: Temps never builds this service.
        preset: 'dockerfile',
        directory: './',
        main_branch: 'main',
        source_type: 'manual',
        automatic_deploy: false,
        storage_service_ids: [],
      },
    })
  }

  return (
    <>
      <Form {...form}>
        <form
          onSubmit={(event) => void form.handleSubmit(submit)(event)}
          className="space-y-4"
        >
          <div className="flex items-start gap-3">
            <Globe className="mt-0.5 size-5 shrink-0 text-muted-foreground" />
            <div className="space-y-1">
              <p className="text-sm font-medium">
                Track an app hosted elsewhere
              </p>
              <p className="text-sm text-muted-foreground">
                For an app on Vercel, Netlify, a VPS or anywhere else. Temps
                receives its analytics, session replays, errors and traces, but
                never deploys it. The service is labelled External; you can let
                Temps host it later in its settings.
              </p>
            </div>
          </div>
          <FormField
            control={form.control}
            name="name"
            render={({ field }) => (
              <FormItem>
                <FormLabel htmlFor="external-project-name">
                  Service name
                </FormLabel>
                <FormControl>
                  <Input
                    {...field}
                    id="external-project-name"
                    placeholder="marketing-site"
                    autoFocus
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          {create.isError && (
            <p role="alert" className="text-sm text-destructive">
              {getErrorMessage(
                create.error,
                'The service could not be created.'
              )}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={onCancel}>
              Cancel
            </Button>
            <Button type="submit" disabled={!name.trim() || create.isPending}>
              {create.isPending && <Loader2 className="size-4 animate-spin" />}
              Create service
            </Button>
          </div>
        </form>
      </Form>
      {joinGroupDialog}
    </>
  )
}
