// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@temps-sdk/ds'
import { Badge, Card, CardContent, CardHeader } from '@temps-sdk/ui'
import { Activity, ArrowRight, Box, Clock, Mail, Rocket } from 'lucide-react'
import { Link } from 'react-router'

const choices = [
  {
    title: 'Deploy an app',
    icon: Rocket,
    estimate: '5–10 min',
    description:
      'Create a project from a starter template and deploy your app.',
    prerequisite: 'Choose a template. Deployment needs an available worker.',
    action: 'Create project',
    href: '/projects/new?source=templates',
  },
  {
    title: 'Send email',
    icon: Mail,
    estimate: '5–10 min',
    description: 'Connect an email provider and send your first test message.',
    prerequisite: 'Needs provider credentials and a verified sender domain.',
    action: 'Set up email',
    href: '/email',
  },
  {
    title: 'Run a sandbox',
    icon: Box,
    estimate: '3–5 min',
    description:
      'Create an isolated environment, then use its terminal or API.',
    prerequisite: 'Needs an available worker. No AI provider is required.',
    action: 'Create sandbox',
    href: '/sandboxes?new=sandbox',
  },
  {
    title: 'Monitor an app',
    icon: Activity,
    estimate: '5–15 min',
    description:
      'Connect an existing app for errors, analytics or OpenTelemetry.',
    prerequisite: 'Needs access to your app’s SDK or exporter configuration.',
    action: 'Connect app',
    href: '/projects/new?source=monitor',
  },
] as const

/** Navigation only: each feature's existing flow owns setup and progress. */
export function OnboardingChoices({
  showHeading = true,
}: {
  showHeading?: boolean
}) {
  const ChoiceHeading = showHeading ? 'h3' : 'h2'
  return (
    <section aria-label="Get started with Temps" className="min-w-0 space-y-4">
      {showHeading && (
        <h2 className="text-lg font-semibold tracking-tight">
          What would you like to try first?
        </h2>
      )}
      <nav
        aria-label="Choose a starting point"
        className="grid gap-4 md:grid-cols-2"
      >
        {choices.map((choice) => (
          <Card key={choice.href} className="flex min-w-0 flex-col">
            <CardHeader className="space-y-3">
              <div className="flex flex-wrap items-center gap-3">
                <choice.icon className="size-5 shrink-0" aria-hidden />
                <ChoiceHeading className="text-base font-semibold">
                  {choice.title}
                </ChoiceHeading>
                <Badge variant="outline" className="gap-1.5 sm:ml-auto">
                  <Clock className="size-3" aria-hidden />
                  <span>
                    <span className="sr-only">Estimated hands-on effort: </span>
                    {choice.estimate}
                  </span>
                </Badge>
              </div>
              <p className="text-sm text-muted-foreground">
                {choice.description}
              </p>
            </CardHeader>
            <CardContent className="flex flex-1 flex-col items-start gap-4">
              <p className="text-sm text-muted-foreground">
                {choice.prerequisite}
              </p>
              <Button asChild className="mt-auto min-h-11">
                <Link to={choice.href}>
                  {choice.action} <ArrowRight className="size-4" aria-hidden />
                </Link>
              </Button>
            </CardContent>
          </Card>
        ))}
      </nav>
      <p className="text-xs leading-relaxed text-muted-foreground">
        Estimated hands-on effort with prerequisites ready; not a
        completion-time guarantee. Templates and required services vary. Builds,
        downloads, DNS verification and app releases can add waiting time.
      </p>
      <div className="flex flex-wrap items-center gap-4 border-t pt-4 text-sm">
        <Link to="/resources" className="underline underline-offset-4">
          Just exploring
        </Link>
        <Link
          to="/projects/new"
          className="text-muted-foreground underline underline-offset-4"
        >
          Other ways to create a project
        </Link>
      </div>
    </section>
  )
}
