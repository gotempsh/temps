// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useNavigate } from 'react-router'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import {
  createSandboxMutation,
  getSandboxPlacementOptions,
  listSandboxesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
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
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { problemDetail } from '@/lib/api-problem'
import { cn } from '@/lib/utils'
import {
  AUTO_NODE,
  PREVIEW_PASSWORD_MIN_LEN,
  createSandboxFormDefaults,
  createSandboxFormSchema,
  toCreateSandboxBody,
  type CreateSandboxFormValues,
} from './sandbox-create'

const LIFECYCLES = [
  {
    value: 'ephemeral',
    title: 'Ephemeral',
    description:
      'A throwaway environment. It is destroyed when the idle timeout passes.',
  },
  {
    value: 'workspace',
    title: 'Workspace',
    description:
      'A long-lived place to work. It sleeps when idle and wakes on the next use; nothing deletes it automatically.',
  },
] as const

interface CreateSandboxSheetProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /**
   * False when this server's profile runs no sandboxes. The sheet still opens
   * and says so, rather than leaving a dead button.
   */
  available: boolean
}

/**
 * "New sandbox": the console counterpart of `temps sandbox create` /
 * `POST /v1/sandboxes`. Only the fields that are filled in are sent, so
 * everything left empty keeps the platform default.
 */
export function CreateSandboxSheet({
  open,
  onOpenChange,
  available,
}: CreateSandboxSheetProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const form = useForm<CreateSandboxFormValues>({
    resolver: zodResolver(createSandboxFormSchema),
    defaultValues: createSandboxFormDefaults,
  })

  const create = useMutation({
    ...createSandboxMutation(),
    meta: { errorTitle: 'Failed to create sandbox' },
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: listSandboxesQueryKey() })
      toast.success(`Sandbox ${response.sandbox.name} created`)
      onOpenChange(false)
      navigate(`/sandboxes/${response.sandbox.id}`)
    },
  })
  const { reset: resetCreate } = create

  // Start from a clean form, and without the previous attempt's error, each
  // time the sheet is opened (it stays mounted while closed).
  useEffect(() => {
    if (!open) return
    form.reset(createSandboxFormDefaults)
    resetCreate()
  }, [open, form, resetCreate])

  const placement = useQuery({
    ...getSandboxPlacementOptions(),
    enabled: open && available,
    retry: false,
  })
  const nodes = (placement.data?.nodes ?? []).filter((node) => node.eligible)

  const submit = (values: CreateSandboxFormValues) =>
    create.mutate({ body: toCreateSandboxBody(values) })

  return (
    <Sheet
      open={open}
      onOpenChange={(next) => {
        if (create.isPending) return
        onOpenChange(next)
      }}
    >
      <SheetContent className="flex w-full flex-col gap-0 overflow-y-auto sm:max-w-lg">
        <SheetHeader>
          <SheetTitle>New sandbox</SheetTitle>
          <SheetDescription>
            Start an isolated compute environment. Leave a field empty to use
            the platform default.
          </SheetDescription>
        </SheetHeader>

        {!available && (
          <Alert className="mt-4">
            <AlertDescription>
              Sandboxes are not available on this server. Its profile runs no
              local workloads, so a new sandbox cannot be created here.
            </AlertDescription>
          </Alert>
        )}

        <Form {...form}>
          <form
            onSubmit={form.handleSubmit(submit)}
            className="mt-4 flex flex-1 flex-col gap-5"
            noValidate
          >
            <FormField
              control={form.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Name</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="my-sandbox"
                      autoComplete="off"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    Optional. Shown in the list and used by the CLI.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="lifecycle"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Lifecycle</FormLabel>
                  <FormControl>
                    <RadioGroup
                      value={field.value}
                      onValueChange={field.onChange}
                      className="grid gap-2 sm:grid-cols-2"
                    >
                      {LIFECYCLES.map((option) => (
                        <Label
                          key={option.value}
                          htmlFor={`lifecycle-${option.value}`}
                          className={cn(
                            'flex cursor-pointer items-start gap-3 rounded-lg border p-3 font-normal',
                            field.value === option.value &&
                              'border-primary bg-primary/5'
                          )}
                        >
                          <RadioGroupItem
                            id={`lifecycle-${option.value}`}
                            value={option.value}
                            className="mt-0.5"
                          />
                          <span className="space-y-1">
                            <span className="block text-sm font-medium">
                              {option.title}
                            </span>
                            <span className="block text-xs text-muted-foreground">
                              {option.description}
                            </span>
                          </span>
                        </Label>
                      ))}
                    </RadioGroup>
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="image"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Image</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="platform default"
                      autoComplete="off"
                      className="font-mono"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    A container image reference. Leave empty for the default
                    sandbox image, which includes the terminal agent.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <div className="grid gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="timeoutMinutes"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Idle timeout (minutes)</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="platform default"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="ports"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Ports</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="3000, 8080"
                        autoComplete="off"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="cpuLimit"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>CPU limit (cores)</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="decimal"
                        placeholder="platform default"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="memoryLimitMb"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Memory limit (MB)</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="platform default"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="pidsLimit"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Process limit</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="platform default"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="node"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Node</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value={AUTO_NODE}>
                          Automatic placement
                        </SelectItem>
                        {nodes.map((node) => (
                          <SelectItem key={node.id} value={node.name}>
                            {node.name} ({node.live_sandboxes} running)
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>

            <FormField
              control={form.control}
              name="previewPassword"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Preview password</FormLabel>
                  <FormControl>
                    <Input
                      type="password"
                      autoComplete="new-password"
                      placeholder="optional"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    Gates every preview URL behind a login and lets you create
                    shareable preview links. At least {PREVIEW_PASSWORD_MIN_LEN}{' '}
                    characters. Without one, the sandbox ID is the only gate.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            {create.isError && (
              <Alert variant="destructive">
                <AlertDescription>
                  {problemDetail(
                    create.error,
                    'The sandbox could not be created.'
                  )}
                </AlertDescription>
              </Alert>
            )}

            <SheetFooter className="mt-auto gap-2 pt-2 sm:justify-end">
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={create.isPending}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending || !available}>
                {create.isPending && (
                  <Loader2 className="mr-2 size-4 animate-spin" />
                )}
                Create sandbox
              </Button>
            </SheetFooter>
          </form>
        </Form>
      </SheetContent>
    </Sheet>
  )
}
