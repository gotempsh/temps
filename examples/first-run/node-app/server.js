// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use strict";

// Managed-service authentication probe app for the first-run scenario suite
// (apps/temps-e2e `first-run-scenario`). It is deployed by uploading this
// directory as source, so Temps has to detect a Node preset and build it.
//
//   GET /        -> {"app":"first-run-node"}
//   GET /health  -> {"status":"ok"}
//   GET /env     -> which managed-service variables are present and whether
//                   each connection URL authenticates and can execute a command.
//                   Values are never echoed back: they carry credentials.

const http = require("http");
const { Client } = require("pg");
const { createClient } = require("redis");

const PORT = parseInt(process.env.PORT || "3000", 10);
const SERVICE_VARIABLES = ["POSTGRES_URL", "REDIS_URL"];

async function probe(value) {
  let target;
  try {
    target = new URL(value);
  } catch {
    return { present: true, parsed: false, reachable: false };
  }
  let client;
  let timer;
  try {
    const deadline = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("probe timed out")), 3000);
    });
    const authenticate = async () => {
      if (["postgres:", "postgresql:"].includes(target.protocol)) {
        client = new Client({
          connectionString: value,
          connectionTimeoutMillis: 3000,
          query_timeout: 3000,
        });
        await client.connect();
        await client.query("SELECT 1");
      } else if (["redis:", "rediss:"].includes(target.protocol)) {
        client = createClient({
          url: value,
          socket: { connectTimeout: 3000, reconnectStrategy: false },
        });
        // Avoid uncaught EventEmitter errors, and never log credential-bearing URLs.
        client.on("error", () => {});
        await client.connect();
        await client.ping();
      } else {
        return { present: true, parsed: false, reachable: false };
      }
    };
    const supported = [
      "postgres:",
      "postgresql:",
      "redis:",
      "rediss:",
    ].includes(target.protocol);
    if (!supported) return { present: true, parsed: false, reachable: false };
    await Promise.race([authenticate(), deadline]);
    return { present: true, parsed: true, reachable: true };
  } catch {
    return { present: true, parsed: true, reachable: false };
  } finally {
    clearTimeout(timer);
    if (client instanceof Client) await client.end().catch(() => {});
    else if (client?.isOpen) client.destroy();
  }
}

module.exports = { probe };

async function environmentReport() {
  const report = {};
  for (const name of SERVICE_VARIABLES) {
    const value = process.env[name];
    report[name] = value
      ? await probe(value)
      : { present: false, parsed: false, reachable: false };
  }
  return report;
}

function send(res, status, body) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

if (require.main === module)
  http
    .createServer(async (req, res) => {
      const path = new URL(req.url, "http://localhost").pathname;
      if (path === "/health") return send(res, 200, { status: "ok" });
      if (path === "/env") return send(res, 200, await environmentReport());
      if (path === "/") return send(res, 200, { app: "first-run-node" });
      return send(res, 404, { error: "not found" });
    })
    .listen(PORT, () => {
      console.log(
        JSON.stringify({
          level: "info",
          msg: "first-run-node listening",
          port: PORT,
        }),
      );
    });
