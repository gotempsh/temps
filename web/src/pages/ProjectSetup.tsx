// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useProjectSetup } from '@/hooks/useProjectSetup'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import {
  ArrowRight,
  BadgeCheck,
  Check,
  CheckCircle2,
  Sparkles,
} from 'lucide-react'
import { Link } from 'react-router'

export function ProjectSetup({ project }: { project: ProjectResponse }) {
  const setup = useProjectSetup(project)
  usePageTitle(`${project.name} setup`)

  if (setup.isLoading) {
    return <ProjectSetupSkeleton />
  }

  if (setup.isError) {
    return (
      <div className="mx-auto max-w-5xl py-4">
        <ErrorAlert
          title="Failed to load project setup"
          description="Temps could not check the current setup state for this project."
          retry={() => setup.refetch()}
        />
      </div>
    )
  }

  return (
    <div className="mx-auto max-w-5xl space-y-6 py-2 sm:py-4">
      <header className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div>
          <div className="mb-2 flex items-center gap-2 text-xs font-medium text-primary">
            <BadgeCheck className="size-4" />
            Production readiness
          </div>
          <h1 className="text-2xl font-semibold tracking-tight sm:text-3xl">
            Project setup
          </h1>
          <p className="mt-1.5 max-w-2xl text-sm leading-relaxed text-muted-foreground">
            Connect the pieces that make {project.name} observable, reachable,
            and ready to operate in production.
          </p>
        </div>
        <div className="flex items-baseline gap-1 text-right tabular-nums">
          <span className="text-3xl font-semibold tracking-tight">
            {setup.completedCount}
          </span>
          <span className="text-sm text-muted-foreground">
            / {setup.totalCount} complete
          </span>
        </div>
      </header>

      <Card className="overflow-hidden border-primary/20">
        <CardContent className="p-0">
          <div
            className="p-5 sm:p-6"
            style={{
              backgroundImage:
                'radial-gradient(120% 140% at 0% 0%, color-mix(in oklch, var(--primary) 10%, transparent) 0%, transparent 58%)',
            }}
          >
            <div className="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
              {setup.allDone ? (
                <div className="flex items-start gap-4">
                  <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-emerald-500/10 text-emerald-500">
                    <CheckCircle2 className="size-5" />
                  </div>
                  <div>
                    <p className="text-xs font-medium text-emerald-500">
                      Setup complete
                    </p>
                    <h2 className="mt-0.5 text-lg font-semibold tracking-tight">
                      This project is production-ready
                    </h2>
                    <p className="mt-1 text-sm text-muted-foreground">
                      Every recommended project integration is connected and
                      reporting.
                    </p>
                  </div>
                </div>
              ) : (
                <div className="flex min-w-0 items-start gap-4">
                  <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
                    <Sparkles className="size-5" />
                  </div>
                  <div className="min-w-0">
                    <p className="text-xs font-medium text-primary">
                      Recommended next step
                    </p>
                    <h2 className="mt-0.5 text-lg font-semibold tracking-tight">
                      {setup.nextStep?.title}
                    </h2>
                    <p className="mt-1 max-w-xl text-sm text-muted-foreground">
                      {setup.nextStep?.description}
                    </p>
                  </div>
                </div>
              )}

              {setup.nextStep && (
                <Button asChild size="lg" className="shrink-0">
                  <Link to={setup.nextStep.href}>
                    {setup.nextStep.cta}
                    <ArrowRight className="ml-1.5 size-4" />
                  </Link>
                </Button>
              )}
            </div>

            <div className="mt-5 flex items-center gap-3">
              <div
                className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted"
                role="progressbar"
                aria-label={`${project.name} setup progress`}
                aria-valuemin={0}
                aria-valuemax={setup.totalCount}
                aria-valuenow={setup.completedCount}
              >
                <div
                  className="h-full rounded-full bg-primary transition-all duration-500"
                  style={{ width: `${setup.percent}%` }}
                />
              </div>
              <span className="text-xs font-medium tabular-nums text-muted-foreground">
                {setup.percent}%
              </span>
            </div>
          </div>
        </CardContent>
      </Card>

      <section className="space-y-3">
        <div>
          <h2 className="text-sm font-semibold">All project setup items</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Completed items remain available so you can revisit their
            configuration at any time.
          </p>
        </div>

        <div className="grid gap-3 md:grid-cols-2">
          {setup.steps.map((step, index) => {
            const Icon = step.icon
            return (
              <Link
                key={step.id}
                to={step.href}
                className={cn(
                  'group relative flex min-h-40 flex-col rounded-xl border bg-card p-5 transition-[border-color,background-color,transform] hover:-translate-y-0.5 hover:border-primary/40 hover:bg-accent/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                  step.done && 'border-emerald-500/20'
                )}
              >
                <div className="flex items-start justify-between gap-4">
                  <div
                    className={cn(
                      'flex size-10 items-center justify-center rounded-xl border bg-muted text-muted-foreground',
                      step.done &&
                        'border-emerald-500/20 bg-emerald-500/10 text-emerald-500'
                    )}
                  >
                    {step.done ? (
                      <Check className="size-4" />
                    ) : (
                      <Icon className="size-4" />
                    )}
                  </div>
                  <span className="text-xs font-medium tabular-nums text-muted-foreground">
                    {String(index + 1).padStart(2, '0')}
                  </span>
                </div>

                <div className="mt-5 flex-1">
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-semibold">{step.title}</h3>
                    {step.done && (
                      <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-500">
                        Complete
                      </span>
                    )}
                  </div>
                  <p className="mt-1.5 text-xs leading-relaxed text-muted-foreground">
                    {step.description}
                  </p>
                </div>

                <span className="mt-4 inline-flex items-center gap-1 text-xs font-medium text-primary">
                  {step.done ? 'Review configuration' : step.cta}
                  <ArrowRight className="size-3.5 transition-transform group-hover:translate-x-0.5" />
                </span>
              </Link>
            )
          })}
        </div>
      </section>
    </div>
  )
}

function ProjectSetupSkeleton() {
  return (
    <div className="mx-auto max-w-5xl space-y-6 py-2 sm:py-4">
      <div className="space-y-2">
        <Skeleton className="h-4 w-36" />
        <Skeleton className="h-9 w-52" />
        <Skeleton className="h-4 w-full max-w-xl" />
      </div>
      <Skeleton className="h-44 w-full rounded-xl" />
      <div className="grid gap-3 md:grid-cols-2">
        {Array.from({ length: 6 }).map((_, index) => (
          <Skeleton key={index} className="h-40 rounded-xl" />
        ))}
      </div>
    </div>
  )
}
