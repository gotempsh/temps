// Read TEMPS_API_KEY without depending on @types/node. The openapi-ts
// CLI runs in node/bun, so `globalThis.process` is always available here;
// the cast just keeps the editor's TS service happy.
const env =
  (globalThis as { process?: { env: Record<string, string | undefined> } })
    .process?.env ?? {}

export default {
  client: '@hey-api/client-fetch',
  // input: 'https://app.localup.dev/api-docs/openapi.json',
  input: {
    path: 'http://localhost:8080/api/api-docs/openapi.json',
    fetch: {
      headers: env.TEMPS_API_KEY
        ? { Authorization: `Bearer ${env.TEMPS_API_KEY}` }
        : undefined,
    },
    filters: {
      operations: {
        // Server-Sent Events endpoints: the @tanstack/react-query plugin
        // generates a `const { data } = await ...` mutation wrapper for
        // every operation uniformly, but `client.sse.post()` returns a
        // ServerSentEventsResult (stream), which has no `.data` field —
        // that mismatch fails `tsc`. Neither endpoint has a frontend
        // consumer yet; exclude until real SSE consumption is built and
        // this can be revisited.
        exclude: [
          'POST /projects/{project_id}/ai/conversations/{public_id}/messages',
          'POST /settings/sandbox-rebuild',
        ],
      },
    },
  },
  output: 'src/api/client',
  plugins: ['@tanstack/react-query'],
}
