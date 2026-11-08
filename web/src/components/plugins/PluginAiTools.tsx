// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getPluginAiTools, putPluginAiTools } from '@/api/client/sdk.gen'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { SaveRow, SettingsSection } from '@temps-sdk/ds'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Bot } from 'lucide-react'
import { toast } from 'sonner'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'

const consentSchema = z.object({
  enabled: z.boolean(),
  actor_id: z.string().uuid(),
})

export function PluginAiTools({ name }: { name: string }) {
  const client = useQueryClient()
  const queryKey = ['plugin-ai-tools', name]
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const tools = useQuery({
    queryKey,
    queryFn: async () =>
      (await getPluginAiTools({ path: { name }, throwOnError: true })).data,
    retry: false,
  })
  const form = useForm({
    defaultValues: { enabled: false, actor_id: '' },
    resolver: zodResolver(consentSchema),
  })
  const enabled = useWatch({ control: form.control, name: 'enabled' })
  const dirty = form.formState.isDirty
  useEffect(() => {
    if (tools.data && !dirty)
      form.reset({ enabled: tools.data.enabled, actor_id: tools.data.actor_id })
  }, [tools.data, dirty, form])
  const save = useMutation({
    mutationFn: async (body: { enabled: boolean; actor_id: string }) => {
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
      form.reset({ enabled: data.enabled, actor_id: data.actor_id })
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
  return (
    <SettingsSection
      title="AI chat tools"
      icon={Bot}
      id="setting-plugin-ai-tools"
    >
      {tools.isPending ? (
        <Skeleton className="h-16 w-full" />
      ) : tools.isError ? (
        <div role="alert" className="space-y-2 text-sm">
          <p>
            Could not load AI tools. A platform administrator can manage this
            access.
          </p>
          <Button variant="outline" onClick={() => void tools.refetch()}>
            Retry
          </Button>
        </div>
      ) : tools.data ? (
        <div className="space-y-3">
          <div className="flex items-center gap-3">
            <Switch
              id={`plugin-ai-${name}`}
              checked={enabled}
              aria-disabled={save.isPending}
              onCheckedChange={(value) => {
                if (!save.isPending)
                  form.setValue('enabled', value, { shouldDirty: true })
              }}
            />
            <Label htmlFor={`plugin-ai-${name}`}>
              Allow AI chat to use these tools
            </Label>
          </div>
          <p className="text-sm text-muted-foreground">
            Enabling lets AI chat read plugin data as the signed-in user. Write
            tools use the existing approval flow. Plugin permissions still
            apply.
          </p>
          {!tools.data.enabled && (
            <p className="text-sm text-muted-foreground">
              AI access is off. Enable it to ask questions such as “show my
              latest reports”.
            </p>
          )}
          {save.isError && (
            <p role="alert" className="text-sm text-destructive">
              Could not update AI access. Your previous setting is unchanged;
              try again.
            </p>
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
                  <p className="text-muted-foreground">{tool.description}</p>
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
  )
}
