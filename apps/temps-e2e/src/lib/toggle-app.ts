// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A throwaway Go app whose health can be flipped on demand over HTTP -- built
 * for the monitoring scenario, which needs to make a REAL deployed app start
 * failing (for the health checker to actually detect) and then recover,
 * without touching Docker/the host at all. Toggling via a plain POST through
 * the same public URL the health checker itself dials is a realistic proxy
 * for "the app broke" / "the app came back", and works identically whether
 * the harness runs next to the server or against a remote instance.
 *
 * No external Go dependencies (stdlib only), so the build needs no `go mod
 * tidy` network fetch -- see probe-app.ts's DB-specific counterpart for why
 * that matters.
 */
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const GO_MOD = `module toggle

go 1.24
`

const MAIN_GO = `package main

import (
	"encoding/json"
	"log"
	"net/http"
	"sync"
)

var (
	mu      sync.Mutex
	healthy = true
)

func writeState(w http.ResponseWriter) {
	mu.Lock()
	h := healthy
	mu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]bool{"healthy": h})
}

func main() {
	mux := http.NewServeMux()
	respondPerState := func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		h := healthy
		mu.Unlock()
		if !h {
			http.Error(w, "down", http.StatusInternalServerError)
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	}
	mux.HandleFunc("/", respondPerState)
	mux.HandleFunc("/health", respondPerState)
	mux.HandleFunc("/state", func(w http.ResponseWriter, r *http.Request) {
		writeState(w)
	})
	mux.HandleFunc("/toggle", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "POST only", http.StatusMethodNotAllowed)
			return
		}
		state := r.URL.Query().Get("state")
		mu.Lock()
		switch state {
		case "down":
			healthy = false
		case "up":
			healthy = true
		}
		mu.Unlock()
		writeState(w)
	})

	log.Println("toggle app listening on :3000")
	log.Fatal(http.ListenAndServe("0.0.0.0:3000", mux))
}
`

const DOCKERFILE = `FROM golang:1.24-alpine AS build
WORKDIR /src
COPY go.mod main.go ./
RUN CGO_ENABLED=0 GOOS=linux go build -o /server main.go

FROM gcr.io/distroless/static-debian12
COPY --from=build /server /server
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/server"]
`

export const TOGGLE_APP_PORT = 3000

/** Render the toggle app into a scratch build context and `docker build` (+ push) it. */
export async function buildToggleImage(opts: {
  scratchRoot: string
  registry: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, 'toggle-app')
  const imageRef = opts.tag ?? `${opts.registry}/e2e-toggle-app:latest`

  await rm(ctxDir, { recursive: true, force: true })
  await mkdir(ctxDir, { recursive: true })
  await writeFile(join(ctxDir, 'go.mod'), GO_MOD, 'utf8')
  await writeFile(join(ctxDir, 'main.go'), MAIN_GO, 'utf8')
  await writeFile(join(ctxDir, 'Dockerfile'), DOCKERFILE, 'utf8')

  opts.onLog?.(`docker build -t ${imageRef} ${ctxDir}`)
  await runDocker(['build', '--load', '-t', imageRef, ctxDir], opts.onLog, `docker build ${imageRef}`)
  opts.onLog?.(`docker push ${imageRef}`)
  await runDocker(['push', imageRef], opts.onLog, `docker push ${imageRef}`)
  return imageRef
}

/** POST /toggle?state=up|down against a deployed toggle app through the real proxy. */
export async function setToggleState(
  target: { url: string; headers?: Record<string, string> },
  state: 'up' | 'down',
): Promise<boolean> {
  const res = await fetch(`${target.url.replace(/\/+$/, '')}/toggle?state=${state}`, {
    method: 'POST',
    headers: target.headers,
  })
  if (!res.ok) {
    throw new Error(`POST /toggle?state=${state} returned HTTP ${res.status}`)
  }
  const body = (await res.json()) as { healthy: boolean }
  return body.healthy
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
