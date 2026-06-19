/**
 * Deploy and verify the repo's source-based example projects against a live
 * Temps instance. For each selected example this:
 *   1. renders a minimal Dockerfile into a scratch context and `docker build`s it
 *   2. runs the standard deploy → load → verify → teardown lifecycle
 *      (`runScenarioForImage`) against the built image
 *
 * This is how we exercise OTHER project types (Go, Python, Node/NestJS, Vite,
 * Rust, …) end-to-end, not just a single prebuilt hello image.
 */
import { resolve, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { makeClient, resolveConfig } from '../lib/client.ts'
import { runScenarioForImage, type ScenarioResult } from './scenario.ts'
import {
  EXAMPLES,
  DEFAULT_SUBSET,
  findExample,
  buildExampleImage,
  type ExampleProject,
} from '../lib/examples.ts'

export interface ExamplesOptions {
  only?: string[]
  all?: boolean
  registry?: string
  requests?: string
  concurrency?: string
  keep?: boolean
  withDb?: boolean
  deployTimeout?: string
  list?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

/** Locate the repo's examples/ dir relative to this app (apps/temps-e2e). */
function examplesRoot(): string {
  const here = dirname(fileURLToPath(import.meta.url)) // .../apps/temps-e2e/src/commands
  return resolve(here, '../../../../examples')
}

function selectExamples(opts: ExamplesOptions): ExampleProject[] {
  if (opts.only && opts.only.length > 0) {
    const picked: ExampleProject[] = []
    for (const key of opts.only) {
      const ex = findExample(key)
      if (!ex) {
        throw new Error(
          `Unknown example "${key}". Known: ${EXAMPLES.map((e) => e.key).join(', ')}`,
        )
      }
      picked.push(ex)
    }
    return picked
  }
  if (opts.all) return EXAMPLES
  return DEFAULT_SUBSET.map((k) => findExample(k)!).filter(Boolean)
}

export async function examplesCommand(opts: ExamplesOptions): Promise<void> {
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }

  if (opts.list) {
    if (json) {
      process.stdout.write(
        JSON.stringify(
          EXAMPLES.map((e) => ({
            key: e.key,
            label: e.label,
            relDir: e.relDir,
            port: e.port,
            weight: e.weight,
          })),
          null,
          2,
        ) + '\n',
      )
    } else {
      log('Available example projects:')
      for (const e of EXAMPLES) {
        const def = DEFAULT_SUBSET.includes(e.key) ? ' (default)' : ''
        log(`  ${e.key.padEnd(14)} ${e.label.padEnd(28)} [${e.weight}] ${e.relDir}${def}`)
      }
    }
    return
  }

  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const selected = selectExamples(opts)
  const root = examplesRoot()
  const scratch = join(process.env.TMPDIR ?? '/tmp', 'temps-e2e-examples')
  // Temps deploys an image by having the server `docker pull` it, so the built
  // image must live somewhere the server can pull from. The harness builds each
  // example and pushes it to a registry, then deploys by that ref — the exact
  // path a real user follows. A local registry is the simplest fit for local
  // runs; point at any registry the server can reach for multi-node.
  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY

  if (!registry) {
    throw new Error(
      'A registry is required: Temps deploys images by pulling them, so the built ' +
        'image must be pushed somewhere the server can pull from. Pass --registry ' +
        '<host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY. ' +
        'Start a local one with: docker run -d -p 5111:5000 --name temps-e2e-registry registry:2',
    )
  }

  log(`Temps e2e examples  ->  ${cfg.url}`)
  log(`Registry: ${registry}`)
  log(`Examples: ${selected.map((e) => e.key).join(', ')}  (root: ${root})`)

  const results: { example: string; built: boolean; result?: ScenarioResult; error?: string }[] = []

  for (const ex of selected) {
    log(`\n══════════════════════════════════════════`)
    log(`▶ EXAMPLE: ${ex.label}  (${ex.key})`)
    log(`══════════════════════════════════════════`)

    let imageRef: string
    try {
      log(`\n▶ build image`)
      imageRef = await buildExampleImage(ex, {
        examplesRoot: root,
        scratchRoot: scratch,
        registry,
        onLog: (l) => log(`    ${l}`),
      })
      log(`  ✓ built ${imageRef}`)
    } catch (e) {
      log(`  ✗ build failed: ${(e as Error).message}`)
      results.push({ example: ex.key, built: false, error: (e as Error).message })
      continue
    }

    const onProgress = json
      ? undefined
      : (c: number, t: number | undefined) =>
          process.stderr.write(`\r    ${c}${t ? `/${t}` : ''} requests   `)

    const result = await runScenarioForImage(
      client,
      cfg,
      {
        image: imageRef,
        port: ex.port,
        healthPath: ex.healthPath,
        requests: Number(opts.requests ?? '500'),
        concurrency: Number(opts.concurrency ?? '25'),
        withDb: opts.withDb,
        keep: opts.keep,
        deployTimeoutMs: Number(opts.deployTimeout ?? '300000'),
        label: ex.key,
      },
      log,
      onProgress,
    )
    if (!json) process.stderr.write('\n')
    results.push({ example: ex.key, built: true, result })
    log(`\n  ${result.ok ? '✅ ' + ex.key + ' PASSED' : '❌ ' + ex.key + ' FAILED'}`)
  }

  const passed = results.filter((r) => r.built && r.result?.ok).length
  const total = results.length

  if (json) {
    process.stdout.write(
      JSON.stringify({ url: cfg.url, passed, total, results }, null, 2) + '\n',
    )
  } else {
    log(`\n════════════ SUMMARY ════════════`)
    for (const r of results) {
      const status = !r.built
        ? '❌ build failed'
        : r.result?.ok
          ? `✅ pass (${r.result.proxyLogsRecorded} proxy logs)`
          : '❌ scenario failed'
      log(`  ${r.example.padEnd(14)} ${status}`)
    }
    log(`\n${passed}/${total} examples passed`)
  }

  if (passed < total) process.exitCode = 1
}
