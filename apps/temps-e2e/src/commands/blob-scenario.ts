/**
 * Blob storage (RustFS, platform-wide singleton -- same shape as
 * kv-storage's shared Redis) lifecycle against a live Temps instance:
 *   1. ensure blob storage is enabled + healthy (never disabled in
 *      teardown, since it's a shared singleton other concurrent runs/users
 *      may depend on)
 *   2. put -> real content round trip (exact byte match, not just 201)
 *   3. download -> exact content + Content-Type match against what was
 *      actually served, not just what put() echoed back
 *   4. head -> Content-Length/Content-Type headers match, no body
 *   5. list -> prefix-scoped listing returns exactly what this run wrote
 *   6. copy -> new pathname created, byte-identical to the source
 *   7. delete -> exact deleted-count, idempotent second call
 *   8. deleted blob is actually gone (404 on direct HEAD, not just absent
 *      from list)
 *   9. cross-project isolation: identical pathname, different project_id,
 *      different (and non-crossing) content
 *  10. API-key auth requires project_id in the query (400, not a silent
 *      project 0/null) -- same class of check as kv-scenario's
 *  11. teardown (delete the two throwaway projects; leave blob storage
 *      enabled)
 *
 * Three real bugs found and fixed while building this:
 *
 * 1. `blob_put`'s `#[utoipa::path]` never declared its query params
 *    (`pathname`, `content_type`, `add_random_suffix`, `project_id`) at
 *    all, and `blob_list`'s declared params were missing `project_id` --
 *    both existed on the Rust query-extractor structs but were invisible
 *    to the OpenAPI spec, so the generated SDK typed them as `query?:
 *    never` / omitted `project_id`. No client could actually call these
 *    with a project scope. Fixed both `#[utoipa::path]` blocks in
 *    `crates/temps-blob/src/handlers/handler.rs` and regenerated
 *    `packages/api`.
 *
 * 2. `temps-proxy` (`upstream_response_filter`, `crates/temps-proxy/src/
 *    proxy.rs`) unconditionally stripped `Content-Length` from every HEAD
 *    response, regardless of downstream HTTP version. The comment above
 *    the strip explained the real reason for it (HTTP/2 clients treat a
 *    Content-Length on a HEAD response as a promise of body bytes and
 *    error when none arrive), but the code didn't gate on it -- so an
 *    HTTP/1.1 client (this scenario's `blobHead` call, any real HTTP/1.1
 *    client) got a HEAD response with no Content-Length AND no chunked
 *    encoding on a keep-alive connection, meaning no way to tell the
 *    response is complete. Confirmed live: curl hung indefinitely on a
 *    HEAD through the proxy port while the same request against the
 *    console port (same handler, no proxy in front) returned instantly
 *    with the header present. Fixed by only stripping when
 *    `session.is_http2()`.
 *
 * 3. `BlobService::del` (`crates/temps-blob/src/services/blob_service.rs`)
 *    counted every successful S3 `DeleteObject` call as a deletion, but
 *    S3's DeleteObject is idempotent by design -- it returns success
 *    whether or not the key existed. So the documented "number of blobs
 *    deleted" was really "number of delete calls that didn't error," and
 *    a second delete of the same (already-gone) keys kept reporting them
 *    deleted. Fixed by `HeadObject`-checking existence before each delete
 *    so the count reflects what was actually removed.
 */
import {
  blobPut,
  blobDelete,
  blobList,
  blobCopy,
  blobHead,
  blobDownload,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eProject, ensureBlobEnabled, teardown, makeRunId } from '../lib/flows.ts'

export interface BlobScenarioOptions {
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface BlobScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function blobScenarioCommand(opts: BlobScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps blob-storage scenario  ->  ${cfg.url}`)

  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const step = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const t0 = performance.now()
    log(`\n▶ ${name}`)
    try {
      const r = await fn()
      const ms = performance.now() - t0
      steps.push({ step: name, ok: true, ms })
      log(`  ✓ ${name} (${(ms / 1000).toFixed(1)}s)`)
      return r
    } catch (e) {
      const ms = performance.now() - t0
      steps.push({ step: name, ok: false, detail: (e as Error).message, ms })
      log(`  ✗ ${name}: ${(e as Error).message}`)
      throw e
    }
  }

  const projectIds: number[] = []
  const pathname = `${runId}/hello.txt`
  const copyPathname = `${runId}/hello-copy.txt`
  const content = `hello blob e2e content ${runId}`
  const contentType = 'text/plain'

  try {
    await step('ensure blob storage is enabled + healthy', () => ensureBlobEnabled(client))

    const projectA = await step('create project A', () => createE2eProject(client, { name: `${runId}-blob-a` }))
    projectIds.push(projectA.id)
    log(`  project A #${projectA.id} (${projectA.slug})`)

    const projectB = await step('create project B (for isolation check)', () =>
      createE2eProject(client, { name: `${runId}-blob-b` }),
    )
    projectIds.push(projectB.id)
    log(`  project B #${projectB.id} (${projectB.slug})`)

    const putResult = await step('put: upload a blob (exact metadata echoed back)', async () => {
      const res = unwrap(
        await blobPut({
          client,
          query: { pathname, content_type: contentType, project_id: projectA.id },
          body: content,
        }),
        'blobPut',
      )
      if (res.pathname !== pathname) {
        throw new Error(`blobPut returned pathname="${res.pathname}", expected "${pathname}"`)
      }
      if (res.contentType !== contentType) {
        throw new Error(`blobPut returned contentType="${res.contentType}", expected "${contentType}"`)
      }
      if (res.size !== content.length) {
        throw new Error(`blobPut returned size=${res.size}, expected ${content.length}`)
      }
      return res
    })
    log(`  ${putResult.url}  (${putResult.size} bytes)`)

    await step('download: exact byte-for-byte content + Content-Type match', async () => {
      const res = await blobDownload({ client, path: { project_id: projectA.id, path: pathname } })
      if (res.response?.status !== 200) {
        throw new Error(`blobDownload returned HTTP ${res.response?.status}, expected 200`)
      }
      if (res.data !== content) {
        throw new Error(`blobDownload body was ${JSON.stringify(res.data)}, expected ${JSON.stringify(content)}`)
      }
      const ct = res.response.headers.get('content-type')
      if (!ct?.startsWith(contentType)) {
        throw new Error(`blobDownload Content-Type was "${ct}", expected to start with "${contentType}"`)
      }
    })

    await step('head: Content-Length/Content-Type headers, no body', async () => {
      const res = await blobHead({ client, path: { project_id: projectA.id, path: pathname } })
      if (res.response?.status !== 200) {
        throw new Error(`blobHead returned HTTP ${res.response?.status}, expected 200`)
      }
      const len = res.response.headers.get('content-length')
      if (len !== String(content.length)) {
        throw new Error(`blobHead Content-Length was "${len}", expected "${content.length}"`)
      }
      const ct = res.response.headers.get('content-type')
      if (!ct?.startsWith(contentType)) {
        throw new Error(`blobHead Content-Type was "${ct}", expected to start with "${contentType}"`)
      }
    })

    await step('list: prefix-scoped listing returns exactly what this run wrote', async () => {
      const res = unwrap(
        await blobList({ client, query: { prefix: `${runId}/`, project_id: projectA.id } }),
        'blobList',
      )
      const names = res.blobs.map((b) => b.pathname).sort()
      if (JSON.stringify(names) !== JSON.stringify([pathname])) {
        throw new Error(`blobList(prefix=${runId}/) returned ${JSON.stringify(names)}, expected [${pathname}]`)
      }
    })

    await step('copy: new pathname, byte-identical to the source', async () => {
      const copied = unwrap(
        await blobCopy({
          client,
          body: { fromUrl: putResult.url, toPathname: copyPathname, projectId: projectA.id },
        }),
        'blobCopy',
      )
      if (copied.pathname !== copyPathname) {
        throw new Error(`blobCopy returned pathname="${copied.pathname}", expected "${copyPathname}"`)
      }
      const downloaded = await blobDownload({ client, path: { project_id: projectA.id, path: copyPathname } })
      if (downloaded.data !== content) {
        throw new Error(
          `downloaded copy was ${JSON.stringify(downloaded.data)}, expected ${JSON.stringify(content)}`,
        )
      }
    })

    await step('delete: exact deleted-count, idempotent on a second call', async () => {
      const first = unwrap(
        await blobDelete({ client, body: { pathnames: [pathname, copyPathname], projectId: projectA.id } }),
        'blobDelete',
      )
      if (first.deleted !== 2) throw new Error(`blobDelete on 2 existing blobs returned deleted=${first.deleted}, expected 2`)
      const second = unwrap(
        await blobDelete({ client, body: { pathnames: [pathname, copyPathname], projectId: projectA.id } }),
        'blobDelete',
      )
      if (second.deleted !== 0) {
        throw new Error(`blobDelete on already-deleted blobs returned deleted=${second.deleted}, expected 0`)
      }
    })

    await step('a deleted blob is actually gone (404, not just absent from list)', async () => {
      const res = await blobHead({ client, path: { project_id: projectA.id, path: pathname } })
      if (res.response?.status !== 404) {
        throw new Error(`blobHead on a deleted blob returned HTTP ${res.response?.status}, expected 404`)
      }
    })

    await step('cross-project isolation: same pathname, different project, different content', async () => {
      const isoPath = `${runId}/isolation.txt`
      await blobPut({
        client,
        query: { pathname: isoPath, content_type: contentType, project_id: projectA.id },
        body: 'project-A-secret',
      })
      await blobPut({
        client,
        query: { pathname: isoPath, content_type: contentType, project_id: projectB.id },
        body: 'project-B-secret',
      })
      const fromA = await blobDownload({ client, path: { project_id: projectA.id, path: isoPath } })
      const fromB = await blobDownload({ client, path: { project_id: projectB.id, path: isoPath } })
      if (fromA.data !== 'project-A-secret') {
        throw new Error(`project A's own read returned ${JSON.stringify(fromA.data)}, expected "project-A-secret"`)
      }
      if (fromB.data !== 'project-B-secret') {
        throw new Error(
          `project B read "${fromB.data}" for the same literal pathname project A also wrote -- blob storage is not isolating tenants by project_id`,
        )
      }
      await blobDelete({ client, body: { pathnames: [isoPath], projectId: projectA.id } })
      await blobDelete({ client, body: { pathnames: [isoPath], projectId: projectB.id } })
    })

    await step('API-key auth requires project_id in the query (400, not a silent no-op)', async () => {
      const res = await blobList({ client, query: { prefix: `${runId}/` } })
      if (!res.error) {
        throw new Error(
          `blobList with no project_id succeeded (${res.data?.blobs.length ?? 0} blob(s)), expected HTTP 400`,
        )
      }
      const status = res.response?.status
      if (status !== 400) {
        throw new Error(`blobList with no project_id returned HTTP ${status}, expected 400`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')}; blob storage left enabled)`)
    } else {
      const td = await teardown(client, { projectIds })
      log(
        `\n▶ teardown: deleted ${td.deletedProjects} project(s); blob storage left enabled` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: BlobScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ blob-scenario PASSED' : '❌ blob-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
