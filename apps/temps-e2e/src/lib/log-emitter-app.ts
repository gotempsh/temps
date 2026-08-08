/**
 * A throwaway Go app that emits a distinctive, structured JSON log line to
 * stdout or stderr on demand over HTTP -- built for the logs scenario, which
 * needs REAL container log lines flowing through the actual Docker log
 * collector (temps-log-aggregator's CollectorService), not synthetic rows
 * inserted directly into storage. Triggering emission via an HTTP endpoint
 * (rather than only relying on fixed startup output) lets the test control
 * exactly when/what gets logged, so it can search for a run-unique marker
 * instead of racing arbitrary boot-time output.
 *
 * JSON body with a `level` key is what parser.rs's structured-log path
 * expects (see crates/temps-log-aggregator/src/parser.rs) -- it promotes
 * `level`/`msg` to top-level columns and leaves any other keys (like `code`
 * here) in the searchable `fields` JSONB, which this scenario also asserts.
 *
 * No external Go dependencies (stdlib only) -- see toggle-app.ts's doc
 * comment for why that matters.
 */
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const GO_MOD = `module logemitter

go 1.24
`

const MAIN_GO = `package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
)

func emit(level, msg string, code int) {
	line := map[string]interface{}{"level": level, "msg": msg}
	if code != 0 {
		line["code"] = code
	}
	b, _ := json.Marshal(line)
	if level == "error" {
		fmt.Fprintln(os.Stderr, string(b))
	} else {
		fmt.Fprintln(os.Stdout, string(b))
	}
}

func main() {
	mux := http.NewServeMux()
	respondOk := func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	}
	mux.HandleFunc("/", respondOk)
	mux.HandleFunc("/health", respondOk)
	mux.HandleFunc("/emit", func(w http.ResponseWriter, r *http.Request) {
		level := r.URL.Query().Get("level")
		if level == "" {
			level = "info"
		}
		msg := r.URL.Query().Get("msg")
		code := 0
		if c := r.URL.Query().Get("code"); c != "" {
			fmt.Sscanf(c, "%d", &code)
		}
		emit(level, msg, code)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})

	fmt.Println("log-emitter listening on :3000")
	_ = http.ListenAndServe("0.0.0.0:3000", mux)
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

export const LOG_EMITTER_APP_PORT = 3000

/** Render the log-emitter app into a scratch build context and `docker build` (+ push) it. */
export async function buildLogEmitterImage(opts: {
  scratchRoot: string
  registry: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, 'log-emitter-app')
  const imageRef = opts.tag ?? `${opts.registry}/e2e-log-emitter:latest`

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

/** GET /emit?level=&msg=&code= against a deployed log-emitter app through the real proxy. */
export async function emitLog(
  target: { url: string; headers?: Record<string, string> },
  opts: { level: 'info' | 'error'; msg: string; code?: number },
): Promise<void> {
  const params = new URLSearchParams({ level: opts.level, msg: opts.msg })
  if (opts.code) params.set('code', String(opts.code))
  const res = await fetch(`${target.url.replace(/\/+$/, '')}/emit?${params}`, { headers: target.headers })
  if (!res.ok) {
    throw new Error(`GET /emit returned HTTP ${res.status}`)
  }
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
