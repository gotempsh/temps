// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Container, Info } from 'lucide-react'
import { UseFormRegisterReturn } from 'react-hook-form'

interface RegistryMirrorSettingsProps {
  /**
   * The result of the host form's `register('registry_mirror_prefix')` call,
   * not the `register` function itself -- keeps this component decoupled
   * from the host page's full form-values type, which react-hook-form's
   * `Control`/`UseFormRegister` generics otherwise require to match exactly.
   */
  prefixField: UseFormRegisterReturn<'registry_mirror_prefix'>
  /** Current watched value, for the live example below the input. */
  currentPrefix: string | null | undefined
}

/**
 * Distinct from `DockerRegistrySettings` above: that one authenticates pulls
 * to one *named* private registry a user's own image reference already
 * points at. This rewrites Temps' own generated, otherwise-anonymous
 * `docker.io` references (autopack's `FROM node:22-slim`) for operators whose
 * internal registry is a path-prefixing reverse proxy rather than a
 * `registry-mirrors`-compatible pull-through cache.
 */
export function RegistryMirrorSettings({
  prefixField,
  currentPrefix,
}: RegistryMirrorSettingsProps) {
  const example = currentPrefix?.trim()
    ? currentPrefix.trim()
    : 'registry.example.com/docker'

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Container className="h-5 w-5" />
          Registry Mirror / Prefix
        </CardTitle>
        <CardDescription>
          Route base images Temps builds against (autopack&apos;s{' '}
          <code>FROM node:22-slim</code> and similar) through an internal
          registry that rewrites Docker Hub references, instead of pulling
          anonymously from docker.io.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="registry-mirror-prefix">Prefix</Label>
          <Input
            id="registry-mirror-prefix"
            type="text"
            placeholder="registry.example.com/docker"
            autoComplete="off"
            {...prefixField}
          />
          <p className="text-sm text-muted-foreground">
            <code>node:22-slim</code> becomes{' '}
            <code>{example}/node:22-slim</code>. Leave empty to pull anonymously
            from docker.io (the default).
          </p>
        </div>

        <Alert>
          <Info className="h-4 w-4" />
          <AlertDescription>
            Most operators do not need this: if your internal registry can act
            as a Docker <strong>pull-through cache</strong> (mirror protocol),
            configure it directly on the Docker daemon instead (
            <code>registry-mirrors</code> in{' '}
            <code>/etc/docker/daemon.json</code>) — no Temps setting required,
            and it also covers images Temps does not generate. Use the prefix
            above only when your registry is a path-prefixing reverse proxy that
            cannot do that. See{' '}
            <a
              href="https://temps.sh/docs/configure-a-docker-registry-mirror"
              target="_blank"
              rel="noopener noreferrer"
              className="underline"
            >
              Configure a Docker Registry Mirror
            </a>{' '}
            for both options.
          </AlertDescription>
        </Alert>
      </CardContent>
    </Card>
  )
}
