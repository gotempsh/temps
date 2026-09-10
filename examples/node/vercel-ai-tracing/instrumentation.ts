// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * OpenTelemetry instrumentation setup for exporting Vercel AI SDK
 * traces to a Temps instance.
 *
 * This file initializes the OTel Node SDK with an OTLP HTTP exporter
 * pointing at the Temps OTLP ingestion endpoint. Import and call
 * `setupTracing()` before any AI SDK calls.
 *
 * Environment variables:
 *   OTEL_ENDPOINT  - Temps API base URL (default: http://localhost:3000/api)
 *   OTEL_TOKEN     - Bearer auth token (optional)
 *   PROJECT_ID     - Temps project ID (default: 1)
 */

import { NodeSDK } from "@opentelemetry/sdk-node";
import { BatchSpanProcessor } from "@opentelemetry/sdk-trace-node";
import { OTLPTraceExporter } from "@opentelemetry/exporter-trace-otlp-proto";
import { Resource } from "@opentelemetry/resources";

let sdk: NodeSDK | null = null;

export function setupTracing(opts?: {
  endpoint?: string;
  token?: string;
  projectId?: number;
  serviceName?: string;
}) {
  const endpoint =
    opts?.endpoint ?? process.env.OTEL_ENDPOINT ?? "http://localhost:3000/api";
  const token = opts?.token ?? process.env.OTEL_TOKEN;
  const projectId = opts?.projectId ?? Number(process.env.PROJECT_ID ?? "1");
  const serviceName = opts?.serviceName ?? "vercel-ai-app";

  const headers: Record<string, string> = {
    "X-Temps-Project-Id": String(projectId),
  };
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }

  const exporter = new OTLPTraceExporter({
    url: `${endpoint}/otel/v1/traces`,
    headers,
  });

  sdk = new NodeSDK({
    resource: new Resource({
      "service.name": serviceName,
      "service.version": "1.0.0",
      "deployment.environment": "development",
    }),
    spanProcessors: [
      new BatchSpanProcessor(exporter, {
        maxQueueSize: 100,
        maxExportBatchSize: 10,
        scheduledDelayMillis: 1000,
      }),
    ],
  });

  sdk.start();

  console.log(`OTel tracing configured:`);
  console.log(`  Endpoint: ${endpoint}/otel/v1/traces`);
  console.log(`  Project ID: ${projectId}`);
  console.log(`  Auth: ${token ? "set" : "not set"}`);

  return sdk;
}

export async function shutdownTracing() {
  if (sdk) {
    await sdk.shutdown();
    console.log("OTel SDK shut down.");
  }
}
