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
  },
  output: 'src/api/client',
  plugins: ['@tanstack/react-query'],
}
