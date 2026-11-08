// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getPluginAiTools, putPluginAiTools } from '@/api/client/sdk.gen'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { SaveRow, SettingsSection } from '@temps-sdk/ds'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Bot } from 'lucide-react'
import { toast } from 'sonner'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'

const consentSchema = z.object({
  enabled: z.boolean(),
  actor_id: z.string().uuid(),
  installation_revision: z.string().min(1),
})

export function PluginAiTools({ name }: { name: string }) {
  const isAdmin = useIsInstanceAdmin()
  const [open, setOpen] = useState(false)
  const client = useQueryClient()
  const queryKey = ['plugin-ai-tools', name]
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const tools = useQuery({
    queryKey,
    queryFn: async () =>
      (await getPluginAiTools({ path: { name }, throwOnError: true })).data,
    retry: false,
    enabled: isAdmin,
  })
  const form = useForm({
    defaultValues: { enabled: false, actor_id: '', installation_revision: '' },
    resolver: zodResolver(consentSchema),
  })
  const enabled = useWatch({ control: form.control, name: 'enabled' })
  const dirty = form.formState.isDirty
  useEffect(() => {
    if (tools.data && !dirty)
      form.reset({
        enabled: tools.data.enabled,
        actor_id: tools.data.actor_id,
        installation_revision: tools.data.installation_revision,
      })
  }, [tools.data, dirty, form])
  const save = useMutation({
    mutationFn: async (body: z.infer<typeof consentSchema>) => {
      if (!tools.data)
        throw new Error('Load plugin settings before changing AI access.')
      return (
        await putPluginAiTools({
          path: { name },
          body,
          throwOnError: true,
        })
      ).data
    },
    onSuccess: (data) => {
      client.setQueryData(queryKey, data)
      form.reset({
        enabled: data.enabled,
        actor_id: data.actor_id,
        installation_revision: data.installation_revision,
      })
      toast.success('Plugin AI access updated')
    },
  })
  async function toggle(values: z.infer<typeof consentSchema>) {
    try {
      await save.mutateAsync(values)
    } catch (error) {
      handleSensitiveActionError(error, () => void toggle(values))
    }
  }
  async function reloadSettings() {
    const result = await tools.refetch()
    if (result.isSuccess && result.data) {
      form.reset({
        enabled: result.data.enabled,
        actor_id: result.data.actor_id,
        installation_revision: result.data.installation_revision,
      })
      save.reset()
    }
  }
  if (!isAdmin) return null
  return (
    <>
      <section
        id="setting-plugin-ai-tools"
        className="flex flex-wrap items-center gap-2"
      >
        <Button variant="ghost" size="sm" onClick={() => setOpen(true)}>
          <Bot className="mr-2 size-4" aria-hidden="true" />
          AI chat tools
        </Button>
        {tools.isPending ? (
          <Skeleton className="h-4 w-32" />
        ) : (
          <span className="text-xs text-muted-foreground">
            {tools.isError
              ? 'AI tools unavailable'
              : tools.data?.tools.length === 0
                ? 'No AI tools declared'
                : `AI access ${tools.data?.enabled ? 'on' : 'off'} · ${tools.data?.tools.length ?? 0} tools`}
          </span>
        )}
      </section>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-h-[85dvh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>Plugin AI access</DialogTitle>
            <DialogDescription>
              Choose whether AI chat can use the tools declared by this plugin.
            </DialogDescription>
          </DialogHeader>
          <SettingsSection title="Tools and permissions" icon={Bot}>
            {tools.isPending ? (
              <Skeleton className="h-16 w-full" />
            ) : tools.isError ? (
              <div role="alert" className="space-y-2 text-sm">
                <p>
                  Could not load AI tools. A platform administrator can manage
                  this access.
                </p>
                <Button variant="outline" onClick={() => void tools.refetch()}>
                  Retry
                </Button>
              </div>
            ) : tools.data ? (
              <div className="space-y-3">
                <div className="flex items-center gap-3">
                  <Checkbox
                    id={`plugin-ai-${name}`}
                    checked={enabled}
                    aria-disabled={save.isPending}
                    onCheckedChange={(value) => {
                      if (!save.isPending)
                        form.setValue('enabled', value === true, {
                          shouldDirty: true,
                        })
                    }}
                  />
                  <Label htmlFor={`plugin-ai-${name}`}>
                    Allow AI chat to use these tools
                  </Label>
                </div>
                <p className="text-sm text-muted-foreground">
                  Enabling lets AI chat read plugin data as the signed-in user.
                  Write tools use the existing approval flow. Plugin permissions
                  still apply. Updating the plugin binary turns AI access off
                  until you enable it again.
                </p>
                {!tools.data.enabled && (
                  <p className="text-sm text-muted-foreground">
                    AI access is off. Enable it to ask questions such as “show
                    my latest reports”.
                  </p>
                )}
                {save.isError && (
                  <div role="alert" className="space-y-2 text-sm">
                    <p className="text-destructive">
                      Could not update AI access. Reload settings to discard
                      this edit and check the current installation before
                      choosing access again.
                    </p>
                    <Button
                      variant="outline"
                      aria-disabled={tools.isFetching}
                      onClick={() => {
                        if (!tools.isFetching) void reloadSettings()
                      }}
                    >
                      {tools.isFetching
                        ? 'Reloading settings…'
                        : 'Reload settings'}
                    </Button>
                  </div>
                )}
                {tools.data.tools.length === 0 ? (
                  <p className="text-sm text-muted-foreground">
                    This plugin declares no AI tools.
                  </p>
                ) : (
                  <ul className="max-h-48 overflow-y-auto space-y-2">
                    {tools.data.tools.map((tool) => (
                      <li key={tool.name} className="text-sm break-words">
                        <span className="font-medium">{tool.name}</span>{' '}
                        <Badge variant="outline">{tool.access}</Badge>
                        <p className="text-muted-foreground">
                          {tool.description}
                        </p>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ) : null}
            <SaveRow
              dirty={dirty}
              saving={save.isPending}
              disabled={!tools.data || tools.isError}
              onSave={() => void form.handleSubmit(toggle)()}
            />
            {verificationDialog}
          </SettingsSection>
        </DialogContent>
      </Dialog>
    </>
  )
}
