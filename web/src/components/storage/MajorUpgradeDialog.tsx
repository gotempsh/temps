// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { startPgUpgrade, type PgUpgrade } from '@/lib/pg-upgrades'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Loader2 } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

/**
 * Supported upgrade targets keyed by current major version. Each entry is a
 * single-hop upgrade — CNPG-style orchestration only supports strictly
 * increasing major versions and refuses cross-OS-family upgrades.
 */
const UPGRADE_TARGETS: Record<string, { version: string; image: string; label: string }[]> = {
  '16': [
    { version: '17', image: 'postgres:17-bookworm', label: 'PostgreSQL 17 (bookworm)' },
  ],
  '17': [
    { version: '18', image: 'postgres:18-bookworm', label: 'PostgreSQL 18 (bookworm)' },
  ],
}

const IMAGE_VERSION_RE = /(?:^|:)(?:pg)?(\d{2})(?:[-.]|$)/i

function parsePostgresMajor(image: string): string | null {
  const m = image.match(IMAGE_VERSION_RE)
  return m ? m[1] : null
}

function detectOsFamily(image: string): 'alpine' | 'bookworm' | 'bullseye' | 'unknown' {
  const lower = image.toLowerCase()
  if (lower.includes('alpine')) return 'alpine'
  if (lower.includes('bookworm')) return 'bookworm'
  if (lower.includes('bullseye')) return 'bullseye'
  return 'unknown'
}

const schema = z.object({
  from_version: z.string().min(1),
  to_version: z.string().min(1),
  from_image: z.string().min(1),
  to_image: z.string().min(1, 'Target image is required'),
})

type FormValues = z.infer<typeof schema>

interface MajorUpgradeDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  serviceName: string
  currentImage: string
}

export function MajorUpgradeDialog({
  open,
  onOpenChange,
  serviceId,
  serviceName,
  currentImage,
}: MajorUpgradeDialogProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const fromVersion = useMemo(() => parsePostgresMajor(currentImage) ?? '', [currentImage])
  const fromOs = useMemo(() => detectOsFamily(currentImage), [currentImage])

  const candidates = UPGRADE_TARGETS[fromVersion] ?? []

  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      from_version: fromVersion,
      to_version: candidates[0]?.version ?? '',
      from_image: currentImage,
      to_image: candidates[0]?.image ?? '',
    },
  })

  // Re-seed form when the dialog opens against a different service
  useEffect(() => {
    if (open) {
      form.reset({
        from_version: fromVersion,
        to_version: candidates[0]?.version ?? '',
        from_image: currentImage,
        to_image: candidates[0]?.image ?? '',
      })
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, currentImage])

  const toImage = form.watch('to_image')
  const toOs = useMemo(() => detectOsFamily(toImage), [toImage])
  const osMismatch =
    fromOs !== 'unknown' && toOs !== 'unknown' && fromOs !== toOs

  const startMutation = useMutation({
    mutationFn: (values: FormValues) =>
      startPgUpgrade(serviceId, {
        from_version: values.from_version,
        to_version: values.to_version,
        from_image: values.from_image,
        to_image: values.to_image,
      }),
    onSuccess: (upgrade: PgUpgrade) => {
      toast.success(`Upgrade started for ${serviceName}`)
      queryClient.invalidateQueries({ queryKey: ['pg-upgrades', serviceId] })
      onOpenChange(false)
      navigate(`/storage/${serviceId}/upgrades/${upgrade.id}`)
    },
    onError: (error: Error) => {
      toast.error('Failed to start upgrade', {
        description: error.message ?? 'An unexpected error occurred',
      })
    },
  })

  const onSelectTarget = (versionKey: string) => {
    const match = candidates.find((c) => c.version === versionKey)
    if (!match) return
    form.setValue('to_version', match.version, { shouldValidate: true })
    form.setValue('to_image', match.image, { shouldValidate: true })
  }

  const disableSubmit =
    startMutation.isPending ||
    !fromVersion ||
    candidates.length === 0 ||
    osMismatch

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="w-[95vw] max-w-[540px] max-h-[90vh] overflow-y-auto sm:w-full">
        <DialogHeader>
          <DialogTitle className="text-base sm:text-lg break-words">
            Major Version Upgrade — {serviceName}
          </DialogTitle>
          <DialogDescription className="text-xs sm:text-sm">
            Run a declarative pg_upgrade between PostgreSQL major versions.
            A full backup is taken before anything changes, and the old volume
            is retained for 7 days in case you need to roll back.
          </DialogDescription>
        </DialogHeader>

        <Form {...form}>
          <form
            onSubmit={form.handleSubmit((v) => startMutation.mutate(v))}
            className="space-y-4"
          >
            <div className="rounded-lg border bg-muted/50 p-3 space-y-1">
              <p className="text-sm font-medium">Current Image</p>
              <code className="text-xs text-muted-foreground break-all">
                {currentImage || '(unknown)'}
              </code>
              {fromVersion ? (
                <p className="text-xs text-muted-foreground">
                  Detected: PostgreSQL {fromVersion} ({fromOs})
                </p>
              ) : (
                <p className="text-xs text-destructive">
                  Could not detect a PostgreSQL major version from this image.
                </p>
              )}
            </div>

            {candidates.length === 0 && fromVersion ? (
              <div className="rounded-lg border border-yellow-500/20 bg-yellow-500/10 p-3 flex gap-2">
                <AlertTriangle className="h-4 w-4 text-yellow-600 mt-0.5" />
                <p className="text-xs text-yellow-800 dark:text-yellow-200">
                  No supported upgrade targets from PostgreSQL {fromVersion}.
                </p>
              </div>
            ) : null}

            <FormField
              control={form.control}
              name="to_version"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Target Version</FormLabel>
                  <Select
                    value={field.value}
                    onValueChange={onSelectTarget}
                    disabled={startMutation.isPending || candidates.length === 0}
                  >
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue placeholder="Select a target version..." />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      {candidates.map((c) => (
                        <SelectItem key={c.version} value={c.version}>
                          {c.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <FormDescription>
                    The orchestrator only supports single-step upgrades to
                    higher major versions on the same OS family.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="to_image"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Target Image</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      disabled={startMutation.isPending}
                      placeholder="postgres:17-bookworm"
                    />
                  </FormControl>
                  <FormDescription>
                    Override if you run a custom image. Must match the target
                    version above and use the same OS family as the current
                    image.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            {osMismatch ? (
              <div className="rounded-lg border border-red-500/20 bg-red-500/10 p-3 flex gap-2">
                <AlertTriangle className="h-4 w-4 text-red-600 mt-0.5" />
                <p className="text-xs text-red-700 dark:text-red-300">
                  OS family mismatch: {fromOs} → {toOs}. Cross-OS upgrades
                  corrupt PGDATA and are rejected by the backend.
                </p>
              </div>
            ) : null}

            <div className="rounded-lg border border-yellow-500/20 bg-yellow-500/10 p-3 flex gap-2">
              <AlertTriangle className="h-4 w-4 text-yellow-600 mt-0.5" />
              <div className="space-y-1 text-xs">
                <p className="font-medium text-yellow-800 dark:text-yellow-200">
                  What happens next
                </p>
                <ul className="list-disc pl-4 space-y-0.5 text-yellow-700 dark:text-yellow-300">
                  <li>Full backup is taken to your default S3 source.</li>
                  <li>Service is stopped; data volume is snapshotted for rollback.</li>
                  <li>New container runs pg_upgrade, then ANALYZE.</li>
                  <li>Old volume is retained for 7 days, then swept.</li>
                </ul>
              </div>
            </div>

            <DialogFooter className="gap-2 flex-col sm:flex-row">
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={startMutation.isPending}
                className="w-full sm:w-auto"
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={disableSubmit}
                className="w-full sm:w-auto"
              >
                {startMutation.isPending ? (
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                ) : null}
                Start Upgrade
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
