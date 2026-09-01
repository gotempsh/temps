// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Static file serving for plugin embedded UIs.
 *
 * Supports two modes:
 * 1. Filesystem-based: serves from a directory (dev mode)
 * 2. Embedded: serves from an in-memory asset map (compiled binary)
 *
 * Both modes provide proper caching headers and SPA fallback to index.html.
 */

import { existsSync, readFileSync, statSync } from "node:fs";
import { join, extname } from "node:path";
import type { IncomingMessage, ServerResponse } from "node:http";

const MIME_TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
  ".ttf": "font/ttf",
  ".eot": "application/vnd.ms-fontobject",
  ".map": "application/json",
  ".webp": "image/webp",
  ".avif": "image/avif",
  ".txt": "text/plain; charset=utf-8",
  ".xml": "application/xml",
  ".wasm": "application/wasm",
};

// ---------------------------------------------------------------------------
// Embedded asset interface (for compile-time embedding)
// ---------------------------------------------------------------------------

export interface EmbeddedFile {
  content: Buffer;
  contentType: string;
  /** If true, use immutable cache headers (hashed asset filenames). */
  immutable: boolean;
}

export type EmbeddedAssets = Map<string, EmbeddedFile>;

// ---------------------------------------------------------------------------
// Filesystem-based UI handler (dev mode)
// ---------------------------------------------------------------------------

/**
 * Create a request handler that serves static UI assets from a directory.
 *
 * @param distDir - Absolute path to the compiled UI dist directory.
 * @param basePath - URL base path (default: "/ui").
 */
export function createUiHandler(
  distDir: string,
  basePath = "/ui"
): (req: IncomingMessage, res: ServerResponse) => boolean {
  const prefix = basePath.endsWith("/") ? basePath : basePath + "/";
  const prefixNoSlash = prefix.slice(0, -1);

  const indexHtml = join(distDir, "index.html");
  const hasIndex = existsSync(indexHtml);

  return (req: IncomingMessage, res: ServerResponse): boolean => {
    const url = req.url ?? "/";
    const pathname = url.split("?")[0]!;

    if (pathname === prefixNoSlash) {
      res.writeHead(302, { Location: prefix });
      res.end();
      return true;
    }

    if (!pathname.startsWith(prefix)) {
      return false;
    }

    const relativePath = pathname.slice(prefix.length) || "index.html";
    const filePath = join(distDir, relativePath);

    if (!filePath.startsWith(distDir)) {
      res.writeHead(403);
      res.end("Forbidden");
      return true;
    }

    if (existsSync(filePath) && statSync(filePath).isFile()) {
      serveFilesystemFile(filePath, res);
      return true;
    }

    if (hasIndex && !hasFileExtension(relativePath)) {
      serveFilesystemFile(indexHtml, res, true);
      return true;
    }

    res.writeHead(404);
    res.end("Not Found");
    return true;
  };
}

// ---------------------------------------------------------------------------
// Embedded UI handler (compiled binary mode)
// ---------------------------------------------------------------------------

/**
 * Create a request handler that serves UI assets from an in-memory map.
 *
 * Use this with `bun build --compile` where assets are embedded at build time
 * via the `embed-assets.ts` script (analogous to Rust's `include_dir!`).
 *
 * @param assets - Map of relative paths to embedded file data.
 * @param basePath - URL base path (default: "/ui").
 */
export function createEmbeddedUiHandler(
  assets: EmbeddedAssets,
  basePath = "/ui"
): (req: IncomingMessage, res: ServerResponse) => boolean {
  const prefix = basePath.endsWith("/") ? basePath : basePath + "/";
  const prefixNoSlash = prefix.slice(0, -1);

  const hasIndex = assets.has("index.html");

  return (req: IncomingMessage, res: ServerResponse): boolean => {
    const url = req.url ?? "/";
    const pathname = url.split("?")[0]!;

    // Redirect /ui to /ui/
    if (pathname === prefixNoSlash) {
      res.writeHead(302, { Location: prefix });
      res.end();
      return true;
    }

    if (!pathname.startsWith(prefix)) {
      return false;
    }

    const relativePath = pathname.slice(prefix.length) || "index.html";

    // Prevent traversal
    if (relativePath.includes("..")) {
      res.writeHead(403);
      res.end("Forbidden");
      return true;
    }

    // Try exact match
    const file = assets.get(relativePath);
    if (file) {
      serveEmbeddedFile(file, res);
      return true;
    }

    // SPA fallback
    if (hasIndex && !hasFileExtension(relativePath)) {
      const index = assets.get("index.html")!;
      serveEmbeddedFile(
        { ...index, immutable: false },
        res
      );
      return true;
    }

    res.writeHead(404);
    res.end("Not Found");
    return true;
  };
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

function serveFilesystemFile(
  filePath: string,
  res: ServerResponse,
  isFallback = false
): void {
  const ext = extname(filePath).toLowerCase();
  const contentType = MIME_TYPES[ext] ?? "application/octet-stream";
  const content = readFileSync(filePath);

  const isHtml = ext === ".html" || isFallback;
  const cacheControl = isHtml
    ? "no-cache, no-store, must-revalidate"
    : "public, max-age=31536000, immutable";

  res.writeHead(200, {
    "Content-Type": contentType,
    "Content-Length": content.byteLength,
    "Cache-Control": cacheControl,
  });
  res.end(content);
}

function serveEmbeddedFile(
  file: EmbeddedFile,
  res: ServerResponse
): void {
  const cacheControl = file.immutable
    ? "public, max-age=31536000, immutable"
    : "no-cache, no-store, must-revalidate";

  res.writeHead(200, {
    "Content-Type": file.contentType,
    "Content-Length": file.content.byteLength,
    "Cache-Control": cacheControl,
  });
  res.end(file.content);
}

function hasFileExtension(path: string): boolean {
  const lastSegment = path.split("/").pop() ?? "";
  return lastSegment.includes(".");
}
