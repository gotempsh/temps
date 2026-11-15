<p align="center">
  <img src="https://raw.githubusercontent.com/gotempsh/temps/refs/heads/main/web/public/logo/temps-logo-light.png" alt="Temps Platform" width="700" />
</p>

<h1 align="center">@temps-sdk/blob</h1>

<p align="center">
  <a href="https://www.npmjs.com/package/@temps-sdk/blob"><img src="https://img.shields.io/npm/v/@temps-sdk/blob.svg" alt="npm version" /></a>
  <a href="https://www.npmjs.com/package/@temps-sdk/blob"><img src="https://img.shields.io/npm/dm/@temps-sdk/blob.svg" alt="npm downloads" /></a>
  <a href="https://github.com/AnomalyCo/temps/blob/main/LICENSE"><img src="https://img.shields.io/npm/l/@temps-sdk/blob.svg" alt="license" /></a>
</p>

<p align="center">
  File storage for Temps projects. Upload, download, list, copy, and delete files with a simple API -- backed by S3-compatible storage, no configuration required.
</p>

---

```bash
# npm
npm install @temps-sdk/blob

# bun
bun add @temps-sdk/blob

# pnpm
pnpm add @temps-sdk/blob

# yarn
yarn add @temps-sdk/blob
```

## Quick Start

```typescript
import { blob } from '@temps-sdk/blob';

// Upload a file
const { url } = await blob.put('avatars/user-123.png', fileBuffer);

// Download it back
const response = await blob.download(url);
const data = await response.arrayBuffer();

// List files
const { blobs } = await blob.list({ prefix: 'avatars/' });

// Delete it
await blob.del(url);
```

That's it. The `blob` singleton reads `TEMPS_API_URL` and `TEMPS_TOKEN` from your environment automatically.

## Configuration

### Environment Variables

```bash
TEMPS_API_URL=https://your-instance.temps.dev   # Your Temps API URL
TEMPS_TOKEN=your-token                           # API key or deployment token
TEMPS_PROJECT_ID=42                              # Required for API keys, optional for deployment tokens
```

### Explicit Configuration

```typescript
import { createClient } from '@temps-sdk/blob';

const storage = createClient({
  apiUrl: 'https://your-instance.temps.dev',
  token: 'your-token',
  projectId: 42,
});
```

> **Deployment tokens** embed the project ID, so `projectId` is optional. **API keys** require `projectId` to be set explicitly.

## API Reference

### `put(pathname, body, options?): Promise<BlobInfo>`

Upload a file. Content type is auto-detected from the file extension or can be set explicitly.

```typescript
// Upload a string
await blob.put('notes/readme.txt', 'Hello, world!');

// Upload a Buffer (Node.js)
import { readFileSync } from 'fs';
const file = readFileSync('./photo.jpg');
await blob.put('photos/vacation.jpg', file);

// Upload a Uint8Array
const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
await blob.put('data/header.bin', bytes);

// Upload a Blob (browser)
const formBlob = new Blob(['content'], { type: 'text/plain' });
await blob.put('uploads/file.txt', formBlob);

// Upload a ReadableStream
const stream = file.stream();
await blob.put('videos/clip.mp4', stream);
```

**Accepted body types:** `string | ArrayBuffer | Uint8Array | Blob | ReadableStream<Uint8Array> | Buffer`

**Options:**

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `contentType` | `string` | Auto-detected | MIME type of the file |
| `addRandomSuffix` | `boolean` | `false` | Append a random suffix to prevent name collisions |

**Returns `BlobInfo`:**

```typescript
{
  url: string;          // Full URL to access the file
  pathname: string;     // Path/name of the file
  contentType: string;  // MIME type
  size: number;         // Size in bytes
  uploadedAt: string;   // ISO 8601 timestamp
}
```

### `del(urls): Promise<void>`

Delete one or more files by URL or pathname.

```typescript
// Delete a single file
await blob.del(fileUrl);

// Delete multiple files
await blob.del([urlA, urlB, urlC]);
```

### `head(url): Promise<BlobInfo>`

Get metadata about a file without downloading it.

```typescript
const info = await blob.head(fileUrl);

console.log(info.size);        // 1048576
console.log(info.contentType); // 'image/png'
console.log(info.uploadedAt);  // '2025-01-15T10:30:00.000Z'
```

### `list(options?): Promise<ListResult>`

List files with optional prefix filtering and cursor-based pagination.

```typescript
// List all files
const { blobs, hasMore, cursor } = await blob.list();

// List files under a prefix
const images = await blob.list({ prefix: 'images/', limit: 50 });

// Paginate through results
let page = await blob.list({ limit: 100 });
while (page.hasMore) {
  for (const file of page.blobs) {
    console.log(file.pathname, file.size);
  }
  page = await blob.list({ limit: 100, cursor: page.cursor });
}
```

**Options:**

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `limit` | `number` | `1000` | Maximum number of results |
| `prefix` | `string` | - | Filter by path prefix (e.g., `'images/'`) |
| `cursor` | `string` | - | Pagination cursor from a previous response |

### `download(url): Promise<Response>`

Download a file. Returns a standard `Response` object.

```typescript
const response = await blob.download(fileUrl);

// Read as text
const text = await response.text();

// Read as binary
const buffer = await response.arrayBuffer();

// Stream to file (Node.js)
import { writeFile } from 'fs/promises';
const data = Buffer.from(await response.arrayBuffer());
await writeFile('./downloaded.png', data);
```

### `copy(fromUrl, toPathname): Promise<BlobInfo>`

Copy a file to a new location server-side (no re-upload needed).

```typescript
const copy = await blob.copy(originalUrl, 'backups/photo-backup.jpg');
console.log(copy.url); // URL of the new copy
```

## Usage Patterns

### User Avatar Upload (Next.js API Route)

```typescript
import { blob } from '@temps-sdk/blob';

export async function POST(request: Request) {
  const form = await request.formData();
  const file = form.get('avatar') as File;

  const { url } = await blob.put(
    `avatars/${userId}/${file.name}`,
    await file.arrayBuffer(),
    {
      contentType: file.type,
      addRandomSuffix: false,
    }
  );

  return Response.json({ url });
}
```

### Backup and Restore

```typescript
// Backup
const data = JSON.stringify(await db.export());
await blob.put(`backups/${new Date().toISOString()}.json`, data, {
  contentType: 'application/json',
});

// List recent backups
const { blobs } = await blob.list({ prefix: 'backups/', limit: 10 });

// Restore latest
const latest = blobs[blobs.length - 1];
const response = await blob.download(latest.url);
const backup = await response.json();
```

### Static Asset Pipeline

```typescript
import { readFileSync } from 'fs';
import { createHash } from 'crypto';

async function uploadAsset(filePath: string) {
  const content = readFileSync(filePath);
  const hash = createHash('md5').update(content).digest('hex').slice(0, 8);
  const ext = filePath.split('.').pop();

  const { url } = await blob.put(`assets/${hash}.${ext}`, content, {
    addRandomSuffix: false,
  });

  return url;
}
```

### Cleanup Old Files

```typescript
async function cleanupOlderThan(prefix: string, maxAgeMs: number) {
  const { blobs } = await blob.list({ prefix });
  const cutoff = Date.now() - maxAgeMs;

  const stale = blobs.filter(b => new Date(b.uploadedAt).getTime() < cutoff);

  if (stale.length > 0) {
    await blob.del(stale.map(b => b.url));
    console.log(`Deleted ${stale.length} stale files`);
  }
}

await cleanupOlderThan('temp/', 7 * 24 * 60 * 60 * 1000); // 7 days
```

## Multiple Clients

Create separate instances for different use cases:

```typescript
import { BlobClient } from '@temps-sdk/blob';

const publicAssets = new BlobClient({ apiUrl: '...', token: '...' });
const privateData = new BlobClient({ apiUrl: '...', token: '...' });
```

## Error Handling

All errors are instances of `BlobError` with structured details:

```typescript
import { blob, BlobError } from '@temps-sdk/blob';

try {
  await blob.head('nonexistent.txt');
} catch (error) {
  if (error instanceof BlobError) {
    console.error(error.message);  // 'Blob not found: nonexistent.txt'
    console.error(error.code);     // 'NOT_FOUND'
    console.error(error.status);   // 404
    console.error(error.title);    // RFC 7807 problem title
    console.error(error.detail);   // RFC 7807 problem detail
  }
}
```

**Error codes:**

| Code | Description |
|------|-------------|
| `MISSING_CONFIG` | Required environment variable or config option not set |
| `NETWORK_ERROR` | Failed to reach the Temps API |
| `NOT_FOUND` | File does not exist |
| `INVALID_INPUT` | Invalid argument (e.g., empty pathname) |

## Auto-Detected Content Types

The SDK detects MIME types from file extensions automatically:

| Extensions | Content Type |
|---|---|
| `.jpg`, `.jpeg` | `image/jpeg` |
| `.png` | `image/png` |
| `.gif` | `image/gif` |
| `.webp` | `image/webp` |
| `.svg` | `image/svg+xml` |
| `.pdf` | `application/pdf` |
| `.json` | `application/json` |
| `.html` | `text/html` |
| `.css` | `text/css` |
| `.js` | `application/javascript` |
| `.mp4` | `video/mp4` |
| `.mp3` | `audio/mpeg` |
| `.zip` | `application/zip` |
| Others | `application/octet-stream` |

Override with the `contentType` option when auto-detection isn't sufficient.

## TypeScript

Fully typed. All methods return typed responses:

```typescript
import type { BlobInfo, ListResult, PutOptions } from '@temps-sdk/blob';

const info: BlobInfo = await blob.put('file.txt', 'content');
const result: ListResult = await blob.list();
```

## Requirements

- Node.js 18+ or Bun
- A running Temps instance

## Related

- [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) -- Key-value store
- [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) -- React analytics, session replay, error tracking
- [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) -- Full platform API client and server-side error tracking

## License

MIT
