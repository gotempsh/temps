// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { mkdir, writeFile, unlink, rmdir, open } from 'node:fs/promises'
import { resolve, join } from 'node:path'
import type { Command } from 'commander'

const header = '// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\n'
export const workerFiles: Readonly<Record<string, string>> = {
  'package.json': JSON.stringify({ name: 'durable-worker', private: true, type: 'module', scripts: { start: 'node worker.mjs', test: 'node --test worker.test.mjs' }, dependencies: { '@temps-sdk/workflows': '0.1.0' } }, null, 2) + '\n',
  'workflow.mjs': header + `import { defineWorkflow } from '@temps-sdk/workflows'
export const hello = defineWorkflow('hello', async (ctx) => {
  const greeting = await ctx.step('greet', () => ({ message: 'Hello from a durable step' }))
  await ctx.sleep('pause', '1s')
  return greeting
})
`,
  'worker.mjs': header + `import { createServer } from 'node:http'
import { createWorkflowHandler, toNodeListener } from '@temps-sdk/workflows'
import { hello } from './workflow.mjs'
const handler = toNodeListener(createWorkflowHandler({ workflows: [hello] }))
const server = createServer((req, res) => {
  if (req.url === '/health') { res.writeHead(200); res.end('ok'); return }
  void handler(req, res)
})
server.requestTimeout = 120000
server.headersTimeout = 10000
server.listen(Number(process.env.PORT || 3000), '0.0.0.0')
for (const signal of ['SIGTERM', 'SIGINT']) process.once(signal, () => {
  server.close(() => process.exit(0))
  setTimeout(() => process.exit(1), 10000).unref()
})
`,
  'worker.test.mjs': header + `import { test } from 'node:test'
import { strict as assert } from 'node:assert'
import { hello } from './workflow.mjs'
import { createTestRunner } from '@temps-sdk/workflows/testing'
test('worker completes a durable step and resumes after sleeping', async () => {
  const runner = createTestRunner({ workflows: [hello] })
  const run = await runner.start('hello')
  await runner.settle()
  assert.equal((await runner.run(run.id)).status, 'sleeping')
  await runner.advance('2s')
  assert.equal((await runner.run(run.id)).status, 'completed')
  assert.deepEqual((await runner.run(run.id)).output, { message: 'Hello from a durable step' })
})
`,
  'Dockerfile': `# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
FROM node:22-alpine
WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev --ignore-scripts
COPY --chown=node:node . .
USER node
EXPOSE 3000
CMD ["node", "worker.mjs"]
`,
  '.dockerignore': '# SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n# SPDX-License-Identifier: MIT OR Apache-2.0\nnode_modules\n.git\n.env*\n',
  '.gitignore': '# SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n# SPDX-License-Identifier: MIT OR Apache-2.0\nnode_modules/\n.env*\n',
}
export async function scaffoldWorker(directory: string, sdkPackage?: string): Promise<string> {
  let sdk: Buffer | undefined
  if (sdkPackage) {
    const handle = await open(resolve(sdkPackage), 'r')
    try {
      const stat = await handle.stat()
      if (!stat.isFile() || stat.size > 16 * 1024 * 1024) throw new Error('SDK package must be a regular tarball no larger than 16 MiB')
      sdk = Buffer.alloc(stat.size + 1)
      let offset = 0
      while (offset < sdk.length) {
        const { bytesRead } = await handle.read(sdk, offset, sdk.length - offset, null)
        if (!bytesRead) break
        offset += bytesRead
      }
      if (offset > stat.size) throw new Error('SDK package changed while reading')
      sdk = sdk.subarray(0, offset)
      if (sdk[0] !== 0x1f || sdk[1] !== 0x8b) throw new Error('SDK package must be a gzip npm pack tarball')
    } finally { await handle.close() }
  }
  const target = resolve(directory)
  await mkdir(target) // Exclusive: never modify an existing directory, including a symlink.
  const created: string[] = []
  try {
    if (sdk) { await writeFile(join(target, 'workflows-sdk.tgz'), sdk, { flag: 'wx' }); created.push('workflows-sdk.tgz') }
    const files = { ...workerFiles }
    if (sdk) {
      const manifest = JSON.parse(files['package.json']!)
      manifest.dependencies['@temps-sdk/workflows'] = 'file:./workflows-sdk.tgz'
      files['package.json'] = JSON.stringify(manifest, null, 2) + '\n'
      files.Dockerfile = files.Dockerfile!.replace('COPY package.json ./', 'COPY package.json workflows-sdk.tgz ./')
    }
    for (const [name, content] of Object.entries(files)) {
      await writeFile(join(target, name), content, { flag: 'wx' }); created.push(name)
    }
    return target
  } catch (error) {
    for (const name of created) await unlink(join(target, name)).catch(() => {})
    await rmdir(target).catch(() => {})
    throw error
  }
}
export function registerInit(parent: Command): void {
  parent.command('init <directory>').description('Create a Node workflow worker in a new directory')
    .option('--sdk-package <tarball>', 'Use a locally built npm pack tarball before SDK publication')
    .option('--json', 'Output the created path and next steps as JSON')
    .action(async (directory: string, options: { sdkPackage?: string }) => {
      const path = await scaffoldWorker(directory, options.sdkPackage)
      console.log(JSON.stringify({ path, next_steps: ['Install dependencies with npm install and run npm test.', 'Create a workflow credential in the environment console. Store it as the TEMPS_WORKFLOWS_TOKEN runtime secret and set TEMPS_API_URL to the API root.', 'Deploy with the workflow-worker preset; use port 3000 and health path /health.', 'Start the hello workflow from the console or temps workflows start.'] }, null, 2))
    })
}
