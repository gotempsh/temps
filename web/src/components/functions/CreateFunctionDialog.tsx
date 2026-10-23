// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
import { problemDetail } from '@/lib/api-problem'
import {
  buildCreateFunctionRequest,
  bytesToBase64,
  createFunction,
  functionsKeys,
  wasmModuleError,
  type CreateFunctionRequest,
  type FunctionFormValues,
  type FunctionRecord,
  type FunctionsCapability,
} from '@/lib/functions'
import { cn } from '@/lib/utils'

const functionFormSchema = z.object({
  name: z.string(),
  artifact: z.enum(['wasm', 'oci']),
  image: z.string(),
  port: z.string(),
  idle_timeout_seconds: z.string(),
  wake_timeout_seconds: z.string(),
  memory_limit_mb: z.string(),
})

interface WasmFile {
  name: string
  base64: string | null
  error: string | null
}

const ARTIFACTS = [
  {
    value: 'wasm' as const,
    title: 'WASM module',
    description:
      'A WASI preview 1 core module (wasm32-wasip1) serving HTTP CGI-style.',
  },
  {
    value: 'oci' as const,
    title: 'OCI image',
    description: 'A container image that serves HTTP on its port.',
  },
]

function defaults(capability: FunctionsCapability): FunctionFormValues {
  return {
    name: '',
    artifact:
      capability.wasm.available || !capability.oci.available ? 'wasm' : 'oci',
    image: '',
    port: '',
    idle_timeout_seconds: '',
    wake_timeout_seconds: '',
    memory_limit_mb: '',
  }
}

/** Upload a function. Controlled through `open`. */
export function CreateFunctionDialog({
  open,
  onOpenChange,
  projectId,
  capability,
  onForbidden,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  capability: FunctionsCapability
  /** Called with the server's refusal when it answers 403. */
  onForbidden: (error: unknown) => boolean
}) {
  const queryClient = useQueryClient()
  const [wasm, setWasm] = useState<WasmFile | null>(null)
  const form = useForm<FunctionFormValues>({
    resolver: zodResolver(functionFormSchema),
    defaultValues: defaults(capability),
  })
  const artifact = useWatch({ control: form.control, name: 'artifact' })
  const create = useMutation({
    mutationFn: (request: CreateFunctionRequest) =>
      createFunction(projectId, request),
    meta: { errorTitle: 'Could not create the function' },
    onSuccess: async (fn: FunctionRecord) => {
      toast.success(`Function ${fn.name} created`, {
        description: `It starts on its first request to ${fn.hostname}.`,
      })
      await queryClient.invalidateQueries({
        queryKey: functionsKeys.project(projectId),
      })
      close()
    },
    onError: (error) => {
      if (onForbidden(error)) close()
      toast.error('Could not create the function', {
        description: problemDetail(error, 'The server refused the function.'),
      })
    },
  })

  function close() {
    form.reset(defaults(capability))
    setWasm(null)
    onOpenChange(false)
  }

  async function readModule(file: File | undefined) {
    if (!file) {
      setWasm(null)
      return
    }
    const bytes = new Uint8Array(await file.arrayBuffer())
    const error = wasmModuleError(bytes)
    setWasm({
      name: file.name,
      base64: error ? null : bytesToBase64(bytes),
      error,
    })
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) close()
        else onOpenChange(true)
      }}
    >
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>New function</DialogTitle>
          <DialogDescription>
            It gets its own hostname, starts on its first request and sleeps
            when idle.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            noValidate
            className="space-y-4"
            onSubmit={form.handleSubmit((values) => {
              const result = buildCreateFunctionRequest(
                values,
                wasm?.base64 ?? null
              )
              if (!result.ok) {
                if (result.field === 'wasm') {
                  setWasm({
                    name: wasm?.name ?? '',
                    base64: null,
                    error: wasm?.error ?? result.error,
                  })
                } else {
                  form.setError(result.field, { message: result.error })
                }
                return
              }
              create.mutate(result.value)
            })}
          >
            <FormField
              control={form.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Name</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="resize-image"
                      autoComplete="off"
                      spellCheck={false}
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    Lowercase letters, digits and single inner hyphens; part of
                    the hostname.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <div
              role="radiogroup"
              aria-label="Artifact"
              className="grid grid-cols-1 gap-3 sm:grid-cols-2"
            >
              {ARTIFACTS.map((option) => {
                const runtime = capability[option.value]
                return (
                  <button
                    key={option.value}
                    type="button"
                    role="radio"
                    aria-checked={artifact === option.value}
                    disabled={!runtime.available}
                    onClick={() => form.setValue('artifact', option.value)}
                    className={cn(
                      'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:cursor-not-allowed disabled:opacity-60',
                      artifact === option.value && 'border-primary bg-primary/5'
                    )}
                  >
                    <div className="font-medium">{option.title}</div>
                    <p className="mt-1 text-sm text-muted-foreground">
                      {runtime.available
                        ? option.description
                        : (runtime.reason ?? 'No runtime runs it here.')}
                    </p>
                  </button>
                )
              })}
            </div>
            {artifact === 'wasm' ? (
              <div className="space-y-1.5">
                <label htmlFor="function-wasm" className="text-sm font-medium">
                  Module (.wasm)
                </label>
                <Input
                  id="function-wasm"
                  type="file"
                  accept=".wasm,application/wasm"
                  onChange={(event) => void readModule(event.target.files?.[0])}
                  aria-invalid={wasm?.error ? true : undefined}
                />
                {wasm?.error ? (
                  <p className="text-sm text-destructive" role="alert">
                    {wasm.error}
                  </p>
                ) : (
                  <p className="text-sm text-muted-foreground">
                    Up to 32 MB. The request body arrives on stdin and the
                    response is read from stdout.
                  </p>
                )}
              </div>
            ) : (
              <FormField
                control={form.control}
                name="image"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Image</FormLabel>
                    <FormControl>
                      <Input
                        className="font-mono"
                        placeholder="registry.example.com/team/fn:1"
                        autoComplete="off"
                        spellCheck={false}
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="port"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Port</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="8080"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="memory_limit_mb"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Memory limit (MiB)</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="No limit"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="idle_timeout_seconds"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Sleep after idle (seconds)</FormLabel>
                    <FormControl>
                      <Input inputMode="numeric" placeholder="300" {...field} />
                    </FormControl>
                    <FormDescription>10 to 86400.</FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="wake_timeout_seconds"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Wake timeout (seconds)</FormLabel>
                    <FormControl>
                      <Input inputMode="numeric" placeholder="30" {...field} />
                    </FormControl>
                    <FormDescription>
                      How long the first request waits, 1 to 300.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={create.isPending}
                onClick={close}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : null}
                Create function
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
