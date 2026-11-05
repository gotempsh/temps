// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createStatusPageMutation,
  publishStatusPageMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ChoiceList } from '@/components/ui/choice-list'
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
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { SaveRow, SettingsGroup } from '@temps-sdk/ds'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { StatusPageComponentsEditor } from './StatusPageComponentsEditor'
import {
  createDefaults,
  createStatusPageSchema,
  operationQueries,
  statusPageUrl,
  toSelection,
  type CreateStatusPageValues,
} from './status-page-form'

type Visibility = 'publish' | 'draft'

const VISIBILITY_CHOICES = [
  {
    value: 'publish' as const,
    label: 'Publish now',
    description: 'Visitors can open the page as soon as it is created.',
  },
  {
    value: 'draft' as const,
    label: 'Save as draft',
    description:
      'Nothing is public yet. Preview and style the page, then publish it.',
  },
]

/**
 * A new status page: its title, the components it shows and whether it goes
 * live straight away, saved as one form.
 */
export function CreateStatusPageForm({
  initialProjectId,
}: {
  /** The service the user came from: the picker lists it first. */
  initialProjectId?: number
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const form = useForm<CreateStatusPageValues>({
    resolver: zodResolver(createStatusPageSchema),
    defaultValues: createDefaults('', []),
  })
  const publishOnCreate = useWatch({ control: form.control, name: 'publish' })
  const create = useMutation({
    ...createStatusPageMutation(),
    meta: { errorTitle: 'Failed to create status page' },
  })
  const publish = useMutation({
    ...publishStatusPageMutation(),
    meta: { errorTitle: 'Created, but publishing failed' },
  })
  const busy =
    create.isPending || publish.isPending || form.formState.isSubmitting

  const handleCreate = async (values: CreateStatusPageValues) => {
    let id: number
    try {
      const page = await create.mutateAsync({
        body: { title: values.title, monitors: toSelection(values.components) },
      })
      id = page.id
    } catch {
      // Reported by the mutation's error toast; the edits stay in the form.
      return
    }
    let published = false
    if (values.publish) {
      try {
        await publish.mutateAsync({ path: { status_page_id: id } })
        published = true
      } catch {
        // Reported by the mutation's error toast; the draft still exists.
      }
    }
    await queryClient.invalidateQueries(operationQueries('listStatusPages'))
    toast.success(
      published
        ? 'Status page is live. Share the public link from the overview.'
        : `Draft created with ${values.components.length} component(s). Publish when ready.`
    )
    // Back should not return to a form for a page that now exists.
    navigate(statusPageUrl(id), { replace: true })
  }

  return (
    <Form {...form}>
      <form
        className="min-w-0"
        onSubmit={form.handleSubmit(async (values) => {
          if (!busy) await handleCreate(values)
        })}
      >
        <div className="divide-y [&>section]:py-8 [&>section:first-child]:pt-0">
          <SettingsGroup
            title="Page"
            description="The name visitors see at the top of the page."
          >
            <FormField
              control={form.control}
              name="title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Title</FormLabel>
                  <FormControl>
                    <Input placeholder="Acme status" {...field} />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
          </SettingsGroup>

          <SettingsGroup
            title="Components"
            description="The services visitors see. Each shows its live state, 90-day uptime and response times under the public name you choose, never its URL or errors."
          >
            <FormField
              control={form.control}
              name="components"
              render={({ field, fieldState }) => (
                <FormItem>
                  <StatusPageComponentsEditor
                    value={field.value}
                    onChange={field.onChange}
                    errors={form.formState.errors.components}
                    initialProjectId={initialProjectId}
                  />
                  {fieldState.error?.message && (
                    <p
                      role="alert"
                      className="text-sm font-medium text-destructive"
                    >
                      {fieldState.error.message}
                    </p>
                  )}
                </FormItem>
              )}
            />
          </SettingsGroup>

          <SettingsGroup title="Visibility">
            <FormField
              control={form.control}
              name="publish"
              render={({ field }) => (
                <FormItem>
                  <ChoiceList<Visibility>
                    label="Visibility"
                    value={field.value ? 'publish' : 'draft'}
                    onValueChange={(next) => field.onChange(next === 'publish')}
                    choices={VISIBILITY_CHOICES}
                    disabled={busy}
                  />
                </FormItem>
              )}
            />
          </SettingsGroup>
        </div>

        <SaveRow
          sticky
          dirty={form.formState.isDirty}
          saving={busy}
          hint={
            publishOnCreate
              ? 'The page goes live when you create it.'
              : 'The page stays private until you publish it.'
          }
          label={publishOnCreate ? 'Create and publish' : 'Create draft'}
        />
      </form>
    </Form>
  )
}
