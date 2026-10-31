// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { Globe, Loader2, Server } from 'lucide-react'
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
import { ChoiceList, type Choice } from '@/components/ui/choice-list'
import { getErrorMessage } from '@/utils/errorHandling'

const OPTIONS: Choice<Hosting>[] = [
  {
    value: 'hosted',
    label: 'Temps runs it',
    description: 'Temps builds and deploys the app from the source below.',
    icon: Server,
  },
  {
    value: 'external',
    label: 'It runs elsewhere',
    description:
      'The app is hosted outside Temps (Vercel, Netlify, a VPS). Temps receives its analytics, errors and traces, and never deploys it. It is labelled External.',
    icon: Globe,
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

  const changed = selected !== current
  return (
    <Card id="setting-where-it-runs" className="scroll-mt-4">
      <CardHeader>
        <CardTitle>Where it runs</CardTitle>
        <CardDescription>
          Whether Temps hosts this app or only observes it.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <ChoiceList
          label="Where it runs"
          value={selected}
          onValueChange={setSelected}
          choices={OPTIONS}
          disabled={saving}
        />
      </CardContent>
      <CardFooter>
        <p>
          {changed && selected === 'external' ? (
            <>
              Running deployments keep running; Temps refuses new ones. Send its
              analytics with an ingest key from{' '}
              <Link
                className="underline"
                to={`/projects/${project.slug}/analytics`}
              >
                Analytics
              </Link>
              .
            </>
          ) : changed ? (
            'Temps can deploy it again once you save.'
          ) : null}
        </p>
        <Button
          size="sm"
          onClick={() => void save()}
          disabled={!changed || saving}
        >
          {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          Save
        </Button>
      </CardFooter>
    </Card>
  )
}
