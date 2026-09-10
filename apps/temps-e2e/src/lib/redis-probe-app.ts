// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A throwaway Go app that proves a linked managed Redis service is actually
 * writable, not just linked in the DB row. On boot it connects with whatever
 * `REDIS_URL` the deployment injects (the redis provider's runtime env var —
 * includes the per-project/env database number, e.g.
 * `redis://:pass@redis-my-svc:6379/3`). It exposes three write endpoints so
 * the e2e scenario can write distinct, recognisable keys of different Redis
 * types (string, hash, list) before and after a backup, then verify exactly
 * which keys survive after a restore.
 *
 * There is no write API anywhere in the platform's query service
 * (`query_handlers.rs` is read-only row browsing) — deploying a small app
 * that writes via the real injected `REDIS_URL` is the only way to generate
 * genuine key-level data end to end.
 *
 * Deliberately NOT added to the repo's `examples/` tree (test-only fixture).
 * Rendered into a scratch build context the same way `buildProbeImage` does.
 */
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const GO_MOD = `module redis-probe

go 1.24

require github.com/redis/go-redis/v9 v9.7.0
`

const MAIN_GO = `package main

import (
	"context"
	"encoding/json"
	"log"
	"net/http"
	"os"

	"github.com/redis/go-redis/v9"
)

var rdb *redis.Client
var ctx = context.Background()

func main() {
	redisURL := os.Getenv("REDIS_URL")
	if redisURL == "" {
		log.Fatal("REDIS_URL not set")
	}

	opts, err := redis.ParseURL(redisURL)
	if err != nil {
		log.Fatalf("parse redis url %q: %v", redisURL, err)
	}
	rdb = redis.NewClient(opts)

	if _, err := rdb.Ping(ctx).Result(); err != nil {
		log.Fatalf("redis ping: %v", err)
	}
	log.Printf("connected to redis db=%d", opts.DB)

	mux := http.NewServeMux()

	// Root — used as deploy health sentinel before the app registers with the
	// healthcheck route below.
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("redis-probe"))
	})

	// Health — standard probe path; also verifies a live Redis PING.
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		if _, err := rdb.Ping(ctx).Result(); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})

	// Write a string key: GET /write?key=X&value=Y
	mux.HandleFunc("/write", func(w http.ResponseWriter, r *http.Request) {
		key := r.URL.Query().Get("key")
		value := r.URL.Query().Get("value")
		if key == "" {
			http.Error(w, "key required", http.StatusBadRequest)
			return
		}
		if err := rdb.Set(ctx, key, value, 0).Err(); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]string{"key": key, "value": value, "type": "string"})
	})

	// Write a hash field: GET /hset?key=X&field=F&value=V
	mux.HandleFunc("/hset", func(w http.ResponseWriter, r *http.Request) {
		key := r.URL.Query().Get("key")
		field := r.URL.Query().Get("field")
		value := r.URL.Query().Get("value")
		if key == "" || field == "" {
			http.Error(w, "key and field required", http.StatusBadRequest)
			return
		}
		if err := rdb.HSet(ctx, key, field, value).Err(); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]string{"key": key, "field": field, "value": value, "type": "hash"})
	})

	// Push a list element: GET /lpush?key=X&value=V
	mux.HandleFunc("/lpush", func(w http.ResponseWriter, r *http.Request) {
		key := r.URL.Query().Get("key")
		value := r.URL.Query().Get("value")
		if key == "" {
			http.Error(w, "key required", http.StatusBadRequest)
			return
		}
		if err := rdb.LPush(ctx, key, value).Err(); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]string{"key": key, "value": value, "type": "list"})
	})

	port := os.Getenv("PORT")
	if port == "" {
		port = "3000"
	}
	log.Printf("redis-probe listening on :%s", port)
	log.Fatal(http.ListenAndServe("0.0.0.0:"+port, mux))
}
`

const DOCKERFILE = `FROM golang:1.24-alpine AS build
WORKDIR /src
COPY go.mod main.go ./
RUN go mod tidy && CGO_ENABLED=0 GOOS=linux go build -o /server main.go

FROM gcr.io/distroless/static-debian12
COPY --from=build /server /server
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/server"]
`

export const REDIS_PROBE_PORT = 3000
export const REDIS_PROBE_HEALTH_PATH = '/health'

/** Render the Redis probe app into a scratch build context and `docker build` + push it. */
export async function buildRedisProbeImage(opts: {
  scratchRoot: string
  registry: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, 'redis-probe')
  const imageRef = opts.tag ?? `${opts.registry}/e2e-redis-probe:latest`

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
