// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Controller, useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import {
  repositoryInstallSchema as schema,
  repositoryInstallBody,
  type RepositoryInstallValues as Values,
  repositorySelectionValues,
  type RepositorySelection,
} from '@/lib/plugin-repository'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { ChevronDown } from 'lucide-react'
import { PLUGINS_QUERY_KEY } from '@/hooks/usePlugins'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { toast } from 'sonner'
import { useEffect } from 'react'

export type { RepositorySelection } from '@/lib/plugin-repository'

type InstallResult = {
  name: string
  version: string
  source_commit: string
  message: string
}

export function RepositoryInstall({
  disabled,
  onSensitiveError,
  selection,
  onClearSelection,
  onPendingChange,
}: {
  disabled: boolean
  onSensitiveError: (error: unknown, retry: () => void) => boolean
  selection?: RepositorySelection | null
  onClearSelection?: () => void
  onPendingChange?: (pending: boolean) => void
}) {
  const queries = useQueryClient()
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: {
      name: '',
      repository_url: '',
      ref_name: '',
      trusted: false,
    },
  })
  const { reset } = form
  useEffect(() => {
    reset(repositorySelectionValues(selection))
    if (selection)
      document
        .getElementById('repository-install-title')
        ?.scrollIntoView({ block: 'center' })
  }, [selection, reset])
  const install = useMutation({
    mutationFn: async (values: Values) => {
      const body = repositoryInstallBody(values)
      const response = await client.post<{ 200: InstallResult }, unknown, true>(
        {
          url: '/x/plugins/install/repository',
          body,
          headers: { 'Content-Type': 'application/json' },
          throwOnError: true,
        }
      )
      return response.data
    },
    onSuccess: async (result) => {
      toast.success(result.message)
      await queries.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
  useEffect(() => {
    onPendingChange?.(install.isPending)
  }, [install.isPending, onPendingChange])
  async function submit(values: Values) {
    try {
      await install.mutateAsync(values)
    } catch (error) {
      if (onSensitiveError(error, () => void submit(values))) return
      toast.error(
        sensitiveActionErrorMessage(
          error,
          'GitHub installation failed. Check host Git credentials and Docker availability.'
        )
      )
    }
  }
  return (
    <section className="space-y-4" aria-labelledby="repository-install-title">
      <div>
        <h2 id="repository-install-title" className="font-semibold">
          {selection
            ? 'Review installation'
            : 'Install from a custom repository'}
        </h2>
        <p className="text-sm text-muted-foreground">
          {selection ? (
            'Review the selected source and permissions before installing.'
          ) : (
            <>
              Paste a repository URL. Temps detects the plugin name and default
              branch, then compiles an exact commit in Docker. Private
              repositories use Git credentials configured for the host account
              running Temps—not your browser login.
            </>
          )}
        </p>
      </div>
      {selection && (
        <div className="space-y-2 rounded-md border p-3 text-sm">
          <p>
            Reviewing <strong>{selection.name}</strong> from the GitHub catalog.
            Installation is pinned to commit{' '}
            <code className="break-all">{selection.commit}</code>.
          </p>
          <p className="text-muted-foreground">
            A catalog listing is not a security audit. Review the source before
            granting it the host’s permissions.
          </p>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled || install.isPending}
            onClick={onClearSelection}
          >
            Back to catalog
          </Button>
        </div>
      )}
      <form onSubmit={form.handleSubmit(submit)} className="space-y-4">
        <fieldset
          disabled={disabled || install.isPending}
          className="grid gap-4 sm:grid-cols-2"
        >
          <div className="space-y-2 sm:col-span-2">
            <Label htmlFor="plugin-repo">GitHub repository</Label>
            <Input
              id="plugin-repo"
              readOnly={Boolean(selection)}
              placeholder="https://github.com/your-org/your-plugin"
              {...form.register('repository_url')}
            />
            <p className="text-sm text-destructive">
              {form.formState.errors.repository_url?.message}
            </p>
          </div>
          {!selection && (
            <Collapsible className="sm:col-span-2">
              <CollapsibleTrigger asChild>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="group -ml-3"
                >
                  Name and revision{' '}
                  <ChevronDown className="ml-2 h-4 w-4 transition-transform group-data-[state=open]:rotate-180" />
                </Button>
              </CollapsibleTrigger>
              <CollapsibleContent className="grid gap-4 pt-3 sm:grid-cols-2">
                <div className="space-y-2">
                  <Label htmlFor="plugin-name">Plugin name</Label>
                  <Input
                    id="plugin-name"
                    readOnly={Boolean(selection)}
                    placeholder="Auto-detected from package.json"
                    {...form.register('name')}
                  />
                  <p className="text-sm text-destructive">
                    {form.formState.errors.name?.message}
                  </p>
                </div>
                <div className="space-y-2">
                  <Label htmlFor="plugin-ref">Branch, tag, or commit</Label>
                  <Input
                    id="plugin-ref"
                    readOnly={Boolean(selection)}
                    placeholder="Repository default branch"
                    {...form.register('ref_name')}
                  />
                  <p className="text-sm text-destructive">
                    {form.formState.errors.ref_name?.message}
                  </p>
                </div>
              </CollapsibleContent>
            </Collapsible>
          )}
          <div className="flex items-start gap-3 sm:col-span-2">
            <Controller
              control={form.control}
              name="trusted"
              render={({ field }) => (
                <Checkbox
                  id="plugin-trust"
                  className="mt-0.5"
                  checked={field.value}
                  onCheckedChange={(checked) =>
                    field.onChange(checked === true)
                  }
                  onBlur={field.onBlur}
                  ref={field.ref}
                  disabled={disabled || install.isPending}
                />
              )}
            />
            <Label
              htmlFor="plugin-trust"
              className="font-normal leading-relaxed"
            >
              I trust this repository and its dependencies. Installed plugins
              execute code with the Temps host’s permissions; Docker isolates
              the build, not the running plugin.
            </Label>
          </div>
          <p className="text-sm text-destructive sm:col-span-2">
            {form.formState.errors.trusted?.message}
          </p>
          <Button type="submit" className="w-fit">
            {install.isPending
              ? 'Building and installing…'
              : 'Build and install'}
          </Button>
        </fieldset>
      </form>
    </section>
  )
}
