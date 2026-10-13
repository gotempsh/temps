// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { Check, Globe, Loader2, Server } from 'lucide-react'
import type { Hosting, ProjectResponse } from '@/api/client'
import { setProjectHosting } from '@/api/client/sdk.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'

const OPTIONS: {
  value: Hosting
  label: string
  desc: string
  Icon: typeof Server
}[] = [
  {
    value: 'hosted',
    label: 'Temps runs it',
    desc: 'Temps builds and deploys the app from the source below.',
    Icon: Server,
  },
  {
    value: 'external',
    label: 'It runs elsewhere',
    desc: 'The app is hosted outside Temps (Vercel, Netlify, a VPS). Temps receives its analytics, errors and traces, and never deploys it. It is labelled External.',
    Icon: Globe,
  },
]

/** Whether Temps runs this service's app or only observes it. */
export function HostingCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const current = project.hosting
  const [selected, setSelected] = useState<Hosting>(current)
  const [saving, setSaving] = useState(false)
  // Follow changes made elsewhere (the analytics page's "Mark as External",
  // another tab, a refetch after saving). Adjusted while rendering, so the
  // card never paints the stale choice.
  const [syncedCurrent, setSyncedCurrent] = useState(current)
  if (syncedCurrent !== current) {
    setSyncedCurrent(current)
    setSelected(current)
  }

  const save = async () => {
    setSaving(true)
    try {
      await setProjectHosting({
        path: { id: project.id },
        body: { hosting: selected },
        throwOnError: true,
      })
      toast.success(
        selected === 'external'
          ? `${project.name} is now marked as running outside Temps`
          : `Temps can deploy ${project.name} again`
      )
      refetch()
    } catch (err) {
      toast.error(getErrorMessage(err, 'Could not change where it runs'))
    } finally {
      setSaving(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Where it runs</CardTitle>
        <CardDescription>
          Whether Temps hosts this app or only observes it.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {OPTIONS.map((option) => (
          <button
            key={option.value}
            type="button"
            onClick={() => setSelected(option.value)}
            className={cn(
              'flex w-full items-start gap-3 rounded-lg border p-3 text-left transition-colors',
              selected === option.value
                ? 'border-primary ring-1 ring-primary'
                : 'border-border hover:bg-accent'
            )}
          >
            <option.Icon className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 text-sm font-medium">
                {option.label}
                {current === option.value && (
                  <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-normal text-muted-foreground">
                    current
                  </span>
                )}
              </div>
              <div className="text-sm text-muted-foreground">{option.desc}</div>
            </div>
            {selected === option.value && (
              <Check className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
            )}
          </button>
        ))}
        {selected === 'external' && current === 'hosted' && (
          <p className="text-sm text-muted-foreground">
            Deployments that are running now keep running until you stop them;
            Temps refuses new ones. To send its analytics, create an ingest key
            under{' '}
            <Link
              className="underline"
              to={`/projects/${project.slug}/analytics`}
            >
              Analytics
            </Link>
            .
          </p>
        )}
      </CardContent>
      <CardFooter>
        {selected === current ? (
          <p className="text-sm text-muted-foreground">
            {current === 'external'
              ? 'This app runs outside Temps.'
              : 'Temps hosts this app.'}
          </p>
        ) : (
          <Button onClick={() => void save()} disabled={saving}>
            {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {selected === 'external'
              ? 'Mark as running elsewhere'
              : 'Let Temps host it'}
          </Button>
        )}
      </CardFooter>
    </Card>
  )
}
