/**
 * Temps Blob Simple Example
 *
 * This example demonstrates basic blob operations using the @temps-sdk/blob package.
 * It shows how to upload, download, list, and delete blobs.
 *
 * Prerequisites:
 * 1. Copy .env.example to .env
 * 2. Set TEMPS_API_URL to your Temps API URL
 * 3. Set TEMPS_TOKEN to your deployment token
 *    - Create a deployment token: temps tokens create -p <project> --name my-token -y
 *
 * Run with: bun run index.ts
 */

import { blob, createClient, BlobError } from "@temps-sdk/blob";

async function main() {
  console.log("📦 Temps Blob Simple Example\n");

  // ============================================================
  // Method 1: Using the default `blob` instance
  // Uses TEMPS_API_URL and TEMPS_TOKEN from environment variables
  // ============================================================
  console.log("--- Using default blob instance ---\n");

  const testPathname = "example/greeting.json";
  const testContent = JSON.stringify({
    message: "Hello from Temps Blob!",
    timestamp: new Date().toISOString(),
  });

  try {
    // PUT - Upload a blob
    console.log(`Uploading blob to "${testPathname}"...`);
    const putResult = await blob.put(testPathname, testContent, {
      contentType: "application/json",
      addRandomSuffix: false, // Use exact pathname without random suffix
    });
    console.log(`✅ PUT result:`);
    console.log(`   URL: ${putResult.url}`);
    console.log(`   Pathname: ${putResult.pathname}`);
    console.log(`   Size: ${putResult.size} bytes`);
    console.log(`   Content-Type: ${putResult.contentType}`);
    console.log();

    // HEAD - Get blob metadata
    console.log(`Getting metadata for "${putResult.url}"...`);
    const headResult = await blob.head(putResult.url);
    console.log(`✅ HEAD result:`);
    console.log(`   Size: ${headResult.size} bytes`);
    console.log(`   Content-Type: ${headResult.contentType}`);
    console.log(`   Uploaded: ${headResult.uploadedAt}`);
    console.log();

    // DOWNLOAD - Get blob content
    console.log(`Downloading blob from "${putResult.url}"...`);
    const downloadResponse = await blob.download(putResult.url);
    const downloadedContent = await downloadResponse.text();
    console.log(`✅ DOWNLOAD result:`);
    console.log(`   Content: ${downloadedContent}`);
    console.log();

    // LIST - List blobs with prefix
    console.log('Listing blobs with prefix "example/"...');
    const listResult = await blob.list({ prefix: "example/", limit: 10 });
    console.log(`✅ LIST result: ${listResult.blobs.length} blob(s) found`);
    for (const item of listResult.blobs) {
      console.log(`   - ${item.pathname} (${item.size} bytes)`);
    }
    console.log(`   Has more: ${listResult.hasMore}`);
    console.log();

    // Upload a second blob for copy demonstration
    const secondPathname = "example/temp-file.txt";
    console.log(`Uploading second blob to "${secondPathname}"...`);
    const secondBlob = await blob.put(secondPathname, "Temporary content", {
      contentType: "text/plain",
      addRandomSuffix: false,
    });
    console.log(`✅ Second blob uploaded: ${secondBlob.pathname}`);
    console.log();

    // COPY - Copy a blob to a new location
    const copyDestination = "example/copied-file.txt";
    console.log(`Copying blob to "${copyDestination}"...`);
    const copyResult = await blob.copy(secondBlob.url, copyDestination);
    console.log(`✅ COPY result:`);
    console.log(`   New URL: ${copyResult.url}`);
    console.log(`   New Pathname: ${copyResult.pathname}`);
    console.log();

    console.log();
  } catch (error: unknown) {
    if (error instanceof BlobError) {
      console.error("❌ Blob Error:", error.message);
      if (error.title) console.error("   Title:", error.title);
      if (error.detail) console.error("   Detail:", error.detail);
      if (error.code) console.error("   Code:", error.code);
      if (error.status) console.error("   Status:", error.status);
    } else {
      console.error("❌ Unexpected Error:", error);
    }
    process.exit(1);
  }

  // ============================================================
  // Method 2: Using a custom client instance
  // Useful when you need multiple clients or explicit configuration
  // ============================================================
  console.log("--- Using custom client instance ---\n");

  const client = createClient({
    apiUrl: process.env.TEMPS_API_URL,
    token: process.env.TEMPS_TOKEN,
    projectId: process.env.TEMPS_PROJECT_ID
      ? parseInt(process.env.TEMPS_PROJECT_ID, 10)
      : undefined,
  });

  try {
    // Upload binary content (Uint8Array)
    const binaryPathname = "example/binary-data.bin";
    const binaryContent = new Uint8Array([0x48, 0x65, 0x6c, 0x6c, 0x6f]); // "Hello" in bytes

    console.log(`Uploading binary blob to "${binaryPathname}"...`);
    const binaryBlob = await client.put(binaryPathname, binaryContent, {
      contentType: "application/octet-stream",
      addRandomSuffix: false,
    });
    console.log(`✅ Binary blob uploaded: ${binaryBlob.size} bytes`);

    // Download and verify binary content
    const downloadedBinary = await client.download(binaryBlob.url);
    const downloadedBytes = new Uint8Array(
      await downloadedBinary.arrayBuffer()
    );
    const decodedText = new TextDecoder().decode(downloadedBytes);
    console.log(`✅ Downloaded binary content: "${decodedText}"`);

    // Clean up
    await client.del(binaryBlob.url);
    console.log("✅ Cleaned up binary blob");
    console.log();
  } catch (error: unknown) {
    if (error instanceof BlobError) {
      console.error("❌ Blob Error:", error.message);
      if (error.title) console.error("   Title:", error.title);
      if (error.detail) console.error("   Detail:", error.detail);
    } else {
      console.error("❌ Unexpected Error:", error);
    }
    process.exit(1);
  }

  console.log("✨ Example complete!");
}

main();
