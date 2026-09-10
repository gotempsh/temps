// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Next.js instrumentation file for exporting Vercel AI SDK traces to Temps.
 *
 * Place this file at `instrumentation.ts` (or `src/instrumentation.ts`) in
 * your Next.js project root. Next.js automatically loads it on startup.
 *
 * Install dependencies:
 *   bun add @vercel/otel @opentelemetry/exporter-trace-otlp-http @opentelemetry/sdk-trace-node
 *
 * Environment variables (.env.local):
 *   TEMPS_OTEL_ENDPOINT=https://your-temps-instance.example.com/api/otel/v1/traces
 *   TEMPS_AUTH_TOKEN=tk_...
 *   TEMPS_PROJECT_ID=1
 *
 * Then enable telemetry on each AI SDK call:
 *
 *   import { generateText } from "ai";
 *   import { openai } from "@ai-sdk/openai";
 *
 *   const result = await generateText({
 *     model: openai("gpt-4.1-nano"),
 *     prompt: "Hello!",
 *     experimental_telemetry: { isEnabled: true, functionId: "my-chat" },
 *   });
 */

import { registerOTel } from "@vercel/otel";
import { OTLPTraceExporter } from "@opentelemetry/exporter-trace-otlp-proto";
import { BatchSpanProcessor } from "@opentelemetry/sdk-trace-node";

export function register() {
  const tempsEndpoint =
    process.env.TEMPS_OTEL_ENDPOINT ??
    "http://localhost:3000/api/otel/v1/traces";
  const tempsToken = process.env.TEMPS_AUTH_TOKEN;
  const projectId = process.env.TEMPS_PROJECT_ID ?? "1";

  const headers: Record<string, string> = {
    "X-Temps-Project-Id": projectId,
  };
  if (tempsToken) {
    headers["Authorization"] = `Bearer ${tempsToken}`;
  }

  registerOTel({
    serviceName: "my-nextjs-app",
    additionalSpanProcessors: [
      new BatchSpanProcessor(
        new OTLPTraceExporter({
          url: tempsEndpoint,
          headers,
        })
      ),
    ],
  });
}
