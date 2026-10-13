# @temps-sdk/sandbox

Typed TypeScript/JavaScript client for the Temps Sandboxes API.

Drop-in compatible with the `@vercel/sandbox` shape — switch providers by
changing the import and base URL.

## Install

```bash
bun add @temps-sdk/sandbox
# or
npm install @temps-sdk/sandbox
```

## Quick start

```ts
import { Sandbox } from '@temps-sdk/sandbox';

// Reads TEMPS_API_URL and TEMPS_API_TOKEN from the environment when omitted.
const sandbox = await Sandbox.create({
  name: 'my-sandbox',
  timeoutSecs: 7200,
  source: {
    type: 'git',
    url: 'https://github.com/example/repo.git',
    revision: 'main',
  },
});

const { exitCode, stdout } = await sandbox.exec(['node', '--version']);
console.log(stdout);

// Preview a dev server running inside the sandbox.
const url = sandbox.domain(3000); // https://sbx-abc-3000.preview.example.com

await sandbox.stop();
```

## API

- `Sandbox.create(opts)` — create a new sandbox, optionally seeded from a git repo or tarball.
  Pass `node` (a worker node name, id, or `'control-plane'`) to choose where it runs;
  omit it to let Temps place it. `sandbox.info.nodeName` reports where it landed.
- `Sandbox.get(id, config)` — rehydrate an existing sandbox by ID.
- `Sandbox.list(config)` — paginate the caller's sandboxes.
- `sandbox.exec(cmd, opts?)` — synchronous exec; returns stdout/stderr/exitCode.
- `sandbox.execDetached(cmd, opts?)` — background job; poll `jobStatus(jobId)`.
- `sandbox.writeFile({ path, contents, mode? })` — write UTF-8 or binary content.
- `sandbox.readFile(path)` — read as `Uint8Array`.
- `sandbox.stat(path)` / `sandbox.mkdir(path)` — filesystem helpers.
- `sandbox.domain(port)` — build a preview URL for a port exposed inside.
- `sandbox.pause()` / `resume()` / `restart()` / `stop()` / `destroy()` — lifecycle.
- `sandbox.extendTimeout(extraSecs)` — push back the idle expiry.

All errors are instances of `SandboxError`; the `detail` field carries the
RFC 7807 problem description from the server.

## Configuration

| Option        | Env var           | Required |
| ------------- | ----------------- | -------- |
| `apiUrl`      | `TEMPS_API_URL`   | yes      |
| `apiToken`    | `TEMPS_API_TOKEN` | yes      |
| `fetch`       | —                 | no       |

Create a personal access token under **API Keys** in the Temps dashboard.
