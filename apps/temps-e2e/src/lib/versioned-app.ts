// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A throwaway Go app whose ENTIRE response body is a single, build-time-baked
 * version string -- built for the deploy-lifecycle scenario, which needs two
 * genuinely different Docker images (not two Docker tags of the same
 * content) so that "traffic now serves version A again" can be asserted by
 * an EXACT byte-for-byte body match against a real HTTP response, not just a
 * deployment row's status field.
 *
 * The version string is baked in via a `docker build --build-arg
 * VERSION_TEXT=...` -> Go `-ldflags -X` link-time substitution, the standard
 * Go idiom for stamping a build-time value into a binary (same mechanism
 * real CI pipelines use for `-X main.version=$GIT_SHA`). This lets the same
 * source render two distinguishable images (`buildVersionedAppImage` called
 * twice with different `version`s), which is what lets the scenario deploy
 * "version A", then "version B" to the same project/environment and tell
 * them apart on the wire.
 *
 * No external Go dependencies (stdlib only) -- see toggle-app.ts's doc
 * comment for why that matters.
 */
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const GO_MOD = `module versionedapp

go 1.24
`

const MAIN_GO = `package main

import (
	"fmt"
	"net/http"
)

// Set at build time via -ldflags "-X main.versionText=...".
var versionText = "unset"

func main() {
	mux := http.NewServeMux()
	handler := func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(versionText))
	}
	mux.HandleFunc("/", handler)
	mux.HandleFunc("/health", handler)
	fmt.Println("versioned-app listening on :3000, version=" + versionText)
	_ = http.ListenAndServe("0.0.0.0:3000", mux)
}
`

const DOCKERFILE = `FROM golang:1.24-alpine AS build
WORKDIR /src
COPY go.mod main.go ./
ARG VERSION_TEXT=unset
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags "-X main.versionText=\${VERSION_TEXT}" -o /server main.go

FROM gcr.io/distroless/static-debian12
COPY --from=build /server /server
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/server"]
`

export const VERSIONED_APP_PORT = 3000

/**
 * Render the versioned app into a scratch build context and `docker build`
 * (+ push) it with `version` baked in as the exact response body. Callers
 * doing an A/B deploy should call this twice (different `version`, distinct
 * `tag`s) to get two genuinely different images.
 */
export async function buildVersionedAppImage(opts: {
  scratchRoot: string
  registry: string
  /** The exact string the deployed app's "/" and "/health" will respond with. */
  version: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, `versioned-app-${sanitize(opts.version)}`)
  const imageRef = opts.tag ?? `${opts.registry}/e2e-versioned-app-${sanitize(opts.version)}:latest`

  await rm(ctxDir, { recursive: true, force: true })
  await mkdir(ctxDir, { recursive: true })
  await writeFile(join(ctxDir, 'go.mod'), GO_MOD, 'utf8')
  await writeFile(join(ctxDir, 'main.go'), MAIN_GO, 'utf8')
  await writeFile(join(ctxDir, 'Dockerfile'), DOCKERFILE, 'utf8')

  const buildArgs = ['build', '--load', '--build-arg', `VERSION_TEXT=${opts.version}`, '-t', imageRef, ctxDir]
  opts.onLog?.(`docker ${buildArgs.join(' ')}`)
  await runDocker(buildArgs, opts.onLog, `docker build ${imageRef}`)
  opts.onLog?.(`docker push ${imageRef}`)
  await runDocker(['push', imageRef], opts.onLog, `docker push ${imageRef}`)
  return imageRef
}

function sanitize(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9-]/g, '-')
}

/** Spawn a docker subcommand, stream its output through onLog, throw on failure. */
async function runDocker(
  args: string[],
  onLog: ((line: string) => void) | undefined,
  what: string,
): Promise<void> {
  const proc = Bun.spawn(['docker', ...args], { stdout: 'pipe', stderr: 'pipe' })
  const pump = async (stream: ReadableStream<Uint8Array>) => {
    const reader = stream.getReader()
    const decoder = new TextDecoder()
    let buf = ''
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split('\n')
      buf = lines.pop() ?? ''
      for (const l of lines) if (l.trim()) onLog?.(l)
    }
    if (buf.trim()) onLog?.(buf)
  }
  await Promise.all([pump(proc.stdout), pump(proc.stderr)])
  const code = await proc.exited
  if (code !== 0) {
    throw new Error(`${what} failed (exit ${code})`)
  }
}
