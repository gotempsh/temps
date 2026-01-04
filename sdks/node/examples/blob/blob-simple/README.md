# Blob Simple Example

A simple example demonstrating basic blob storage operations using `@temps-sdk/blob`.

## Features Demonstrated

- **PUT**: Upload blobs (text, JSON, binary data)
- **HEAD**: Get blob metadata without downloading content
- **DOWNLOAD**: Retrieve blob content
- **LIST**: List blobs with prefix filtering and pagination
- **COPY**: Copy a blob to a new location
- **DEL**: Delete one or more blobs

## Setup

1. Install dependencies:

```bash
bun install
```

2. Configure environment variables:

```bash
cp .env.example .env
```

3. Edit `.env` with your Temps credentials:

```env
TEMPS_API_URL=https://app.temps.kfs.es
TEMPS_TOKEN=your-api-key-here
# Required when using API keys, optional for deployment tokens
TEMPS_PROJECT_ID=1
```

## Run

```bash
bun run index.ts
# or
bun start
```

## Expected Output

```
📦 Temps Blob Simple Example

--- Using default blob instance ---

Uploading blob to "example/greeting.json"...
✅ PUT result:
   URL: https://app.temps.kfs.es/api/blob/1/example/greeting.json
   Pathname: example/greeting.json
   Size: 89 bytes
   Content-Type: application/json

Getting metadata for "https://..."...
✅ HEAD result:
   Size: 89 bytes
   Content-Type: application/json
   Uploaded: 2024-01-01T00:00:00.000Z

Downloading blob from "https://..."...
✅ DOWNLOAD result:
   Content: {"message":"Hello from Temps Blob!","timestamp":"2024-01-01T00:00:00.000Z"}

Listing blobs with prefix "example/"...
✅ LIST result: 1 blob(s) found
   - example/greeting.json (89 bytes)
   Has more: false

Uploading second blob to "example/temp-file.txt"...
✅ Second blob uploaded: example/temp-file.txt

Copying blob to "example/copied-file.txt"...
✅ COPY result:
   New URL: https://...
   New Pathname: example/copied-file.txt

Deleting all test blobs...
✅ DEL result: All blobs deleted

✅ Verified: Blob was successfully deleted

--- Using custom client instance ---

Uploading binary blob to "example/binary-data.bin"...
✅ Binary blob uploaded: 5 bytes
✅ Downloaded binary content: "Hello"
✅ Cleaned up binary blob

✨ Example complete!
```

## Usage Patterns

### Using the default `blob` instance

```typescript
import { blob } from '@temps-sdk/blob';

// Uses TEMPS_API_URL and TEMPS_TOKEN from environment
const result = await blob.put('path/to/file.txt', 'Hello World');
const content = await blob.download(result.url);
await blob.del(result.url);
```

### Using a custom client

```typescript
import { createClient } from '@temps-sdk/blob';

const client = createClient({
  apiUrl: 'https://app.temps.kfs.es',
  token: 'your-api-key',
  // Required when using API keys, optional for deployment tokens
  projectId: 1,
});

await client.put('path/to/file.txt', 'Hello World');
```

### Uploading different content types

```typescript
// Text/String
await blob.put('docs/readme.txt', 'Plain text content');

// JSON
await blob.put('data/config.json', JSON.stringify({ key: 'value' }), {
  contentType: 'application/json',
});

// Binary data (Uint8Array)
const bytes = new Uint8Array([0x48, 0x65, 0x6c, 0x6c, 0x6f]);
await blob.put('files/data.bin', bytes, {
  contentType: 'application/octet-stream',
});

// File/Blob
const file = new Blob(['file content'], { type: 'text/plain' });
await blob.put('uploads/document.txt', file);
```

### Upload options

```typescript
// Disable random suffix (use exact pathname)
await blob.put('exact/path.txt', 'content', {
  addRandomSuffix: false,
});

// Specify content type
await blob.put('images/photo.jpg', imageBuffer, {
  contentType: 'image/jpeg',
});

// Both options
await blob.put('assets/style.css', cssContent, {
  contentType: 'text/css',
  addRandomSuffix: false,
});
```

### Listing blobs with pagination

```typescript
// List with prefix filter
const result = await blob.list({
  prefix: 'images/',
  limit: 20,
});

console.log(result.blobs);
console.log(result.hasMore);

// Get next page
if (result.hasMore && result.cursor) {
  const nextPage = await blob.list({
    prefix: 'images/',
    cursor: result.cursor,
  });
}
```

### Copying blobs

```typescript
// Copy a blob to a new location
const original = await blob.put('source/file.txt', 'content');
const copy = await blob.copy(original.url, 'destination/file.txt');
```

### Deleting multiple blobs

```typescript
// Delete a single blob
await blob.del('https://app.temps.kfs.es/api/blob/1/path/file.txt');

// Delete multiple blobs at once
await blob.del([
  'https://app.temps.kfs.es/api/blob/1/path/file1.txt',
  'https://app.temps.kfs.es/api/blob/1/path/file2.txt',
]);
```
