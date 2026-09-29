// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { ConnectedRepository } from './ConnectedRepository'
import { StarterLocalFiles } from './StarterLocalFiles'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from '@/components/ui/dialog'
import { SourceLogo } from '@/components/imports/SourceLogo'
import {
  TOP_MIGRATION_SOURCES,
  importHref,
} from '@/components/imports/migration-sources'
import {
  Activity,
  ArrowRight,
  Terminal,
  Sparkles,
  UploadCloud,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { HighlightedCode } from '@/components/ui/code-block'

/** First-run project collection: choose a task before configuring its inputs. */
export function FirstProjectOnboarding() {
  const choosingSource = true
  const origin = typeof window === 'undefined' ? '' : window.location.origin
  const commands = [
    `bunx @temps-sdk/cli login ${origin}`,
    'bunx @temps-sdk/cli up',
  ]

  return (
    <section
      aria-label="Get started with projects"
      className="w-full min-w-0 space-y-6"
    >
      <div className="flex flex-wrap items-center justify-between gap-3 border-b pb-4">
        <p className="text-sm text-muted-foreground">
          Already hosting your application elsewhere?
        </p>
        <Button asChild variant="outline">
          <Link to="/projects/new?source=monitor">
            <Activity className="size-4" /> Monitor an existing application
          </Link>
        </Button>
      </div>
      <div className="grid min-w-0 gap-4 lg:grid-cols-2">
        <ConnectedRepository />
        <div className="min-w-0 self-start rounded-lg border p-5">
          <StarterLocalFiles />
        </div>
      </div>
      <Button asChild variant="ghost">
        <Link to="/projects/new?source=templates&template=observability-starter">
          Try a demo app <ArrowRight className="size-4" />
        </Link>
      </Button>
      {choosingSource && (
        <div className="flex flex-col gap-3 border-t pt-5 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <h2 className="text-sm font-medium">
              Moving from another platform?
            </h2>
            <div
              className="mt-3 flex flex-wrap gap-2"
              aria-label="Migration platforms"
            >
              {TOP_MIGRATION_SOURCES.map((platform) => (
                <Button key={platform.source} asChild variant="ghost" size="sm">
                  <Link
                    to={importHref(platform.source)}
                    aria-label={`Import from ${platform.label}`}
                  >
                    <SourceLogo
                      source={platform.source}
                      className="mr-2 size-5 shrink-0"
                    />
                    {platform.label}
                  </Link>
                </Button>
              ))}
            </div>
          </div>
          <Button asChild variant="outline" className="shrink-0">
            <Link to="/projects/import-wizard">
              Import applications <ArrowRight className="ml-2 size-4" />
            </Link>
          </Button>
        </div>
      )}

      {choosingSource && (
        <nav
          aria-label="Deployment shortcuts"
          className="flex flex-wrap items-center gap-2"
        >
          <Dialog>
            <DialogTrigger asChild>
              <Button variant="ghost" size="sm">
                <Terminal className="mr-2 size-4" /> Use CLI
              </Button>
            </DialogTrigger>
            <DialogContent className="min-w-0">
              <DialogHeader>
                <DialogTitle>Deploy from your terminal</DialogTitle>
                <DialogDescription>
                  Run these commands in your project folder. The CLI opens your
                  browser to sign in, then guides you through deployment.
                </DialogDescription>
              </DialogHeader>
              <div className="min-w-0 space-y-4">
                {commands.map((command, index) => (
                  <div key={command} className="min-w-0 space-y-2">
                    <p className="text-sm font-medium">
                      {index === 0
                        ? '1. Connect to this instance'
                        : '2. Deploy your project'}
                    </p>
                    <div className="flex min-w-0 items-center gap-2 rounded-md border bg-muted/50 p-2 pl-3">
                      <HighlightedCode
                        code={command}
                        language="bash"
                        className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap text-sm"
                      />
                      <CopyButton
                        value={command}
                        minimal
                        aria-label={`Copy ${command}`}
                        className="shrink-0"
                      />
                    </div>
                  </div>
                ))}
              </div>
            </DialogContent>
          </Dialog>
          <Button asChild variant="ghost" size="sm">
            <Link to="/setup/ai">
              <Sparkles className="mr-2 size-4" /> Connect an AI agent
            </Link>
          </Button>
          {!import.meta.env.DEV && (
            <Button asChild variant="ghost" size="sm">
              <Link to="/projects/new?source=drop">
                <UploadCloud className="mr-2 size-4" /> Upload project files
              </Link>
            </Button>
          )}
        </nav>
      )}
    </section>
  )
}
