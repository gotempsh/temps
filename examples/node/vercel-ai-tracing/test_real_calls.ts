#!/usr/bin/env tsx
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Real AI API calls through the Temps AI Gateway with OTel GenAI tracing,
 * using the Vercel AI SDK's built-in telemetry support.
 *
 * The AI SDK automatically emits gen_ai.* OTel spans when you set
 * `experimental_telemetry: { isEnabled: true }`. This script configures
 * the OTLP exporter to send those spans to Temps.
 *
 * Usage:
 *   # Set Temps credentials
 *   export OTEL_ENDPOINT=http://localhost:8081/api
 *   export OTEL_TOKEN=tk_...
 *
 *   # Option A: Use Temps AI Gateway (routes to any configured provider)
 *   npx tsx test_real_calls.ts
 *
 *   # Option B: Direct provider keys (bypass gateway)
 *   export OPENAI_API_KEY=sk-...
 *   export ANTHROPIC_API_KEY=sk-ant-...
 *   npx tsx test_real_calls.ts --direct
 */

import { setupTracing, shutdownTracing } from "./instrumentation.js";

// ── Initialize tracing BEFORE importing AI SDK ─────────────────────
// This ensures the OTel SDK is ready when the AI SDK creates spans.
setupTracing({ serviceName: "ai-gateway-client" });

import { generateText, streamText, tool } from "ai";
import { createOpenAI } from "@ai-sdk/openai";
import { createAnthropic } from "@ai-sdk/anthropic";
import { z } from "zod";

// ── Provider setup ─────────────────────────────────────────────────

const endpoint =
  process.env.OTEL_ENDPOINT ?? "http://localhost:8081/api";
const token = process.env.OTEL_TOKEN;
const direct = process.argv.includes("--direct");

// When using the Temps AI Gateway, all models go through a single
// OpenAI-compatible endpoint. The gateway routes to the right provider.
const gatewayBase = endpoint.replace("/api", "") + "/api/ai/v1";

function getProvider(providerName: string) {
  if (direct) {
    // Direct provider access (requires individual API keys)
    if (providerName === "anthropic") {
      return createAnthropic({});
    }
    return createOpenAI({});
  }

  // Temps AI Gateway: single endpoint, routes by model name
  return createOpenAI({
    apiKey: token ?? "dummy",
    baseURL: gatewayBase,
  });
}

// ── Telemetry config helper ────────────────────────────────────────

function telemetry(functionId: string, metadata?: Record<string, string>) {
  return {
    isEnabled: true as const,
    functionId,
    metadata,
    recordInputs: true,
    recordOutputs: true,
  };
}

// ── Test 1: Simple generateText ────────────────────────────────────

async function testGenerateText() {
  console.log("\n1. generateText — OpenAI gpt-4.1-nano");
  console.log("   " + "-".repeat(50));

  const provider = getProvider("openai");

  const result = await generateText({
    model: provider("gpt-4.1-nano"),
    prompt: "Explain the CAP theorem in distributed systems in 3 sentences.",
    maxTokens: 256,
    experimental_telemetry: telemetry("cap-theorem-explainer", {
      topic: "distributed-systems",
    }),
  });

  console.log(`   Response: ${result.text.slice(0, 120)}...`);
  console.log(
    `   Tokens: ${result.usage.promptTokens} in / ${result.usage.completionTokens} out`
  );
}

// ── Test 2: Anthropic via gateway ──────────────────────────────────

async function testAnthropicChat() {
  console.log("\n2. generateText — Anthropic claude-haiku-4-5");
  console.log("   " + "-".repeat(50));

  const provider = getProvider("anthropic");

  const result = await generateText({
    model: provider("claude-haiku-4-5"),
    prompt:
      "What are the SOLID principles in software engineering? One sentence each.",
    maxTokens: 512,
    experimental_telemetry: telemetry("solid-principles", {
      topic: "software-engineering",
    }),
  });

  console.log(`   Response: ${result.text.slice(0, 120)}...`);
  console.log(
    `   Tokens: ${result.usage.promptTokens} in / ${result.usage.completionTokens} out`
  );
}

// ── Test 3: Streaming ──────────────────────────────────────────────

async function testStreamText() {
  console.log("\n3. streamText — OpenAI gpt-4.1-nano (streaming)");
  console.log("   " + "-".repeat(50));

  const provider = getProvider("openai");

  const result = streamText({
    model: provider("gpt-4.1-nano"),
    prompt: "Write a haiku about Rust programming.",
    maxTokens: 128,
    experimental_telemetry: telemetry("rust-haiku", {
      format: "poetry",
    }),
  });

  let text = "";
  for await (const chunk of result.textStream) {
    text += chunk;
  }

  console.log(`   Response: ${text}`);

  const usage = await result.usage;
  console.log(
    `   Tokens: ${usage.promptTokens} in / ${usage.completionTokens} out`
  );
}

// ── Test 4: System prompt + multi-turn conversation ────────────────

async function testWithSystemPrompt() {
  console.log("\n4. generateText — Multi-turn with system prompt");
  console.log("   " + "-".repeat(50));

  const provider = getProvider("openai");

  const result = await generateText({
    model: provider("gpt-4.1-nano"),
    system: "You are a senior Rust developer. Be concise and precise.",
    messages: [
      { role: "user", content: "What's the difference between Box and Rc?" },
      {
        role: "assistant",
        content:
          "Box is single-ownership heap allocation. Rc is reference-counted shared ownership.",
      },
      {
        role: "user",
        content: "When would I use Arc instead of Rc?",
      },
    ],
    maxTokens: 256,
    experimental_telemetry: telemetry("rust-qa", {
      topic: "rust-ownership",
      turn: "2",
    }),
  });

  console.log(`   Response: ${result.text.slice(0, 120)}...`);
  console.log(
    `   Tokens: ${result.usage.promptTokens} in / ${result.usage.completionTokens} out`
  );
}

// ── Test 5: Tool calling ───────────────────────────────────────────

async function testToolCalling() {
  console.log("\n5. generateText — Tool calling");
  console.log("   " + "-".repeat(50));

  const provider = getProvider("openai");

  const result = await generateText({
    model: provider("gpt-4.1-nano"),
    prompt: "What's the weather in San Francisco?",
    maxTokens: 256,
    tools: {
      getWeather: tool({
        description: "Get the current weather for a city",
        parameters: z.object({
          city: z.string().describe("City name"),
        }),
        execute: async ({ city }) => {
          return { temperature: 62, condition: "Partly cloudy", city };
        },
      }),
    },
    maxSteps: 3,
    experimental_telemetry: telemetry("weather-agent", {
      hasTools: "true",
    }),
  });

  console.log(`   Response: ${result.text.slice(0, 120)}...`);
  console.log(`   Steps: ${result.steps.length}`);
  console.log(
    `   Tokens: ${result.usage.promptTokens} in / ${result.usage.completionTokens} out`
  );
}

// ── Main ───────────────────────────────────────────────────────────

async function main() {
  const projectId = Number(process.env.PROJECT_ID ?? "1");

  console.log(`Temps OTel endpoint: ${endpoint}`);
  console.log(`Temps AI Gateway: ${gatewayBase}`);
  console.log(`Project ID: ${projectId}`);
  console.log(`Mode: ${direct ? "direct provider keys" : "Temps AI Gateway"}`);
  console.log(`Auth: ${token ? "set" : "not set"}`);

  const tests = [
    testGenerateText,
    testAnthropicChat,
    testStreamText,
    testWithSystemPrompt,
    testToolCalling,
  ];

  for (const test of tests) {
    try {
      await test();
    } catch (e: any) {
      console.log(`   ERROR: ${e.message}`);
    }
  }

  console.log("\n\nFlushing OTel spans...");
  await shutdownTracing();

  console.log(
    "\nDone! Emitted 5 real traces via Vercel AI SDK with experimental_telemetry."
  );
  console.log(
    "Check the AI Activity tab in your project to see the traces."
  );
}

main().catch(console.error);
