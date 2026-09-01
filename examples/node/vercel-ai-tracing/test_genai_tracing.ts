#!/usr/bin/env tsx
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Test script that emits OTel GenAI spans to a Temps instance using the
 * Vercel AI SDK. Simulates AI activity so the AI Activity dashboard shows
 * realistic traces — no actual LLM API keys required.
 *
 * The AI SDK automatically emits gen_ai.* semantic convention attributes
 * when `experimental_telemetry` is enabled. We use a mock provider to
 * generate spans without real API calls.
 *
 * Usage:
 *   # Against local Temps instance
 *   npx tsx test_genai_tracing.ts
 *
 *   # Against custom endpoint
 *   OTEL_ENDPOINT=https://my-temps.example.com/api npx tsx test_genai_tracing.ts
 *
 *   # With auth token
 *   OTEL_TOKEN=tk_abc123 npx tsx test_genai_tracing.ts
 *
 *   # Also verify the query endpoints
 *   npx tsx test_genai_tracing.ts --verify
 */

import { setupTracing, shutdownTracing } from "./instrumentation.js";
import { trace, context, SpanKind, SpanStatusCode } from "@opentelemetry/api";

// ── Initialize tracing BEFORE any AI SDK calls ─────────────────────
setupTracing({ serviceName: "test-vercel-ai-agent" });

const tracer = trace.getTracer("test-vercel-ai-agent", "1.0.0");

// ── Helpers to simulate spans matching AI SDK's gen_ai format ──────

function simulateChatCompletion(opts: {
  model: string;
  system: string;
  inputTokens: number;
  outputTokens: number;
  durationMs: number;
  error?: boolean;
  inputMessages?: string;
  outputMessages?: string;
  systemInstructions?: string;
}) {
  const span = tracer.startSpan(`chat ${opts.model}`, {
    kind: SpanKind.CLIENT,
  });

  span.setAttribute("gen_ai.operation.name", "chat");
  span.setAttribute("gen_ai.system", opts.system);
  span.setAttribute("gen_ai.request.model", opts.model);
  span.setAttribute("gen_ai.response.model", opts.model);
  span.setAttribute("gen_ai.usage.input_tokens", opts.inputTokens);
  span.setAttribute("gen_ai.usage.output_tokens", opts.outputTokens);
  span.setAttribute("gen_ai.request.temperature", 0.7);
  span.setAttribute("gen_ai.request.max_tokens", 1024);
  span.setAttribute(
    "gen_ai.response.id",
    `chatcmpl-${crypto.randomUUID().slice(0, 12)}`
  );
  span.setAttribute("gen_ai.response.finish_reasons", ["stop"]);

  if (opts.systemInstructions)
    span.setAttribute("gen_ai.system_instructions", opts.systemInstructions);
  if (opts.inputMessages)
    span.setAttribute("gen_ai.input.messages", opts.inputMessages);
  if (opts.outputMessages)
    span.setAttribute("gen_ai.output.messages", opts.outputMessages);

  if (opts.error) {
    span.setStatus({ code: SpanStatusCode.ERROR, message: "Rate limit exceeded" });
    span.setAttribute("error.type", "RateLimitError");
  } else {
    span.setStatus({ code: SpanStatusCode.OK });
  }

  span.end();
}

function simulateToolExecution(opts: {
  toolName: string;
  durationMs: number;
  arguments?: string;
  result?: string;
}) {
  const span = tracer.startSpan(`execute_tool ${opts.toolName}`, {
    kind: SpanKind.INTERNAL,
  });

  span.setAttribute("gen_ai.operation.name", "execute_tool");
  span.setAttribute("gen_ai.tool.name", opts.toolName);
  span.setAttribute("gen_ai.tool.type", "function");
  span.setAttribute(
    "gen_ai.tool.call.id",
    `call_${crypto.randomUUID().slice(0, 12)}`
  );

  if (opts.arguments)
    span.setAttribute("gen_ai.tool.call.arguments", opts.arguments);
  if (opts.result) span.setAttribute("gen_ai.tool.call.result", opts.result);

  span.setStatus({ code: SpanStatusCode.OK });
  span.end();
}

function simulateEmbeddings(opts: {
  model: string;
  system: string;
  inputTokens: number;
}) {
  const span = tracer.startSpan(`embeddings ${opts.model}`, {
    kind: SpanKind.CLIENT,
  });

  span.setAttribute("gen_ai.operation.name", "embeddings");
  span.setAttribute("gen_ai.system", opts.system);
  span.setAttribute("gen_ai.request.model", opts.model);
  span.setAttribute("gen_ai.usage.input_tokens", opts.inputTokens);

  span.setStatus({ code: SpanStatusCode.OK });
  span.end();
}

// ── Scenario 1: Agent conversation with tool calls ─────────────────

function simulateAgentConversation() {
  console.log("  Simulating agent conversation (OpenAI gpt-5.4)...");

  const agentSpan = tracer.startSpan("invoke_agent research-assistant", {
    kind: SpanKind.INTERNAL,
  });
  const ctx = trace.setSpan(context.active(), agentSpan);

  agentSpan.setAttribute("gen_ai.operation.name", "invoke_agent");
  agentSpan.setAttribute("gen_ai.agent.name", "research-assistant");
  agentSpan.setAttribute("gen_ai.agent.id", "agent-001");
  agentSpan.setAttribute("gen_ai.system", "openai");

  context.with(ctx, () => {
    // First LLM call — decides to use tools
    simulateChatCompletion({
      model: "gpt-5.4",
      system: "openai",
      inputTokens: 250,
      outputTokens: 80,
      durationMs: 50,
      systemInstructions:
        "You are a research assistant. Use tools to look up information when needed.",
      inputMessages: JSON.stringify([
        {
          role: "user",
          content:
            "What's the weather in San Francisco and find me the latest news about AI?",
        },
      ]),
      outputMessages: JSON.stringify([
        {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "call_1",
              function: {
                name: "search_web",
                arguments: '{"query": "latest AI news 2026"}',
              },
            },
            {
              id: "call_2",
              function: {
                name: "get_weather",
                arguments: '{"city": "San Francisco"}',
              },
            },
          ],
        },
      ]),
    });

    // Tool executions
    simulateToolExecution({
      toolName: "search_web",
      durationMs: 30,
      arguments: '{"query": "latest AI news 2026"}',
      result:
        '{"results": [{"title": "OpenAI releases GPT-5", "url": "https://example.com/gpt5"}]}',
    });
    simulateToolExecution({
      toolName: "get_weather",
      durationMs: 20,
      arguments: '{"city": "San Francisco"}',
      result: '{"temperature": 62, "condition": "Partly cloudy", "humidity": 72}',
    });

    // Second LLM call — generates final response
    simulateChatCompletion({
      model: "gpt-5.4",
      system: "openai",
      inputTokens: 600,
      outputTokens: 350,
      durationMs: 80,
      inputMessages: JSON.stringify([
        {
          role: "user",
          content:
            "What's the weather in San Francisco and find me the latest news about AI?",
        },
        {
          role: "tool",
          tool_call_id: "call_1",
          content:
            '{"results": [{"title": "OpenAI releases GPT-5"}, {"title": "Anthropic announces Claude 4"}]}',
        },
        {
          role: "tool",
          tool_call_id: "call_2",
          content: '{"temperature": 62, "condition": "Partly cloudy"}',
        },
      ]),
      outputMessages: JSON.stringify([
        {
          role: "assistant",
          content:
            "Here's what I found:\n\n**Weather in San Francisco:**\nIt's currently 62°F and partly cloudy.\n\n**Latest AI News:**\n1. OpenAI has released GPT-5\n2. Anthropic announced Claude 4",
        },
      ]),
    });
  });

  agentSpan.setStatus({ code: SpanStatusCode.OK });
  agentSpan.end();
}

// ── Scenario 2: Anthropic chat with extended thinking ──────────────

function simulateAnthropicChat() {
  console.log("  Simulating Anthropic chat (claude-sonnet-4-6-20260311)...");
  simulateChatCompletion({
    model: "claude-sonnet-4-6-20260311",
    system: "anthropic",
    inputTokens: 150,
    outputTokens: 420,
    durationMs: 60,
    systemInstructions:
      "You are a helpful coding assistant with access to tools.",
    inputMessages: JSON.stringify([
      {
        role: "user",
        content:
          "What's the current time in Tokyo and write me a haiku about it?",
      },
    ]),
    outputMessages: JSON.stringify([
      {
        role: "assistant",
        content: [
          {
            type: "thinking",
            thinking:
              "The user wants two things: 1) current time in Tokyo, and 2) a haiku about it.",
          },
          {
            type: "text",
            text: "It's currently **4:53 PM JST** in Tokyo.\n\nHere's a haiku:\n\n*Neon lights flicker*\n*Tokyo's afternoon fades*\n*Evening whispers near*",
          },
        ],
      },
    ]),
  });
}

// ── Scenario 3: RAG pipeline ───────────────────────────────────────

function simulateRagPipeline() {
  console.log("  Simulating RAG pipeline (OpenAI)...");

  const ragSpan = tracer.startSpan("rag_query", {
    kind: SpanKind.INTERNAL,
  });
  const ctx = trace.setSpan(context.active(), ragSpan);

  ragSpan.setAttribute("gen_ai.operation.name", "invoke_agent");
  ragSpan.setAttribute("gen_ai.agent.name", "rag-pipeline");
  ragSpan.setAttribute("gen_ai.system", "openai");

  context.with(ctx, () => {
    simulateEmbeddings({
      model: "text-embedding-4",
      system: "openai",
      inputTokens: 45,
    });

    simulateChatCompletion({
      model: "gpt-5.4",
      system: "openai",
      inputTokens: 2000,
      outputTokens: 500,
      durationMs: 100,
      systemInstructions:
        "You are a helpful assistant. Answer questions using the provided context documents.",
      inputMessages: JSON.stringify([
        {
          role: "user",
          content:
            "What are the best practices for deploying machine learning models in production?",
        },
      ]),
      outputMessages: JSON.stringify([
        {
          role: "assistant",
          content:
            "Based on the retrieved documents, here are the best practices for deploying ML models:\n\n1. **Model Versioning** — Use MLflow or DVC\n2. **Containerization** — Docker for consistency\n3. **A/B Testing** — Canary deployments\n4. **Monitoring** — Track drift and latency\n5. **Feature Stores** — Centralized feature management",
        },
      ]),
    });
  });

  ragSpan.setStatus({ code: SpanStatusCode.OK });
  ragSpan.end();
}

// ── Scenario 4: Error trace ────────────────────────────────────────

function simulateErrorTrace() {
  console.log("  Simulating error trace (rate limited)...");
  simulateChatCompletion({
    model: "gpt-5.4",
    system: "openai",
    inputTokens: 100,
    outputTokens: 0,
    durationMs: 10,
    error: true,
    inputMessages: JSON.stringify([
      {
        role: "user",
        content: "Summarize the top 10 most important events in world history.",
      },
    ]),
  });
}

// ── Scenario 5: New gen_ai.provider.name attribute ─────────────────

function simulateNewProviderName() {
  console.log("  Simulating new gen_ai.provider.name attribute (Anthropic)...");
  const span = tracer.startSpan("chat claude-sonnet-4-6-20260311", {
    kind: SpanKind.CLIENT,
  });

  span.setAttribute("gen_ai.operation.name", "chat");
  span.setAttribute("gen_ai.provider.name", "anthropic");
  span.setAttribute("gen_ai.request.model", "claude-sonnet-4-6-20260311");
  span.setAttribute("gen_ai.response.model", "claude-sonnet-4-6-20260311");
  span.setAttribute("gen_ai.usage.input_tokens", 200);
  span.setAttribute("gen_ai.usage.output_tokens", 300);
  span.setAttribute(
    "gen_ai.response.id",
    `msg_${crypto.randomUUID().slice(0, 12)}`
  );
  span.setAttribute("gen_ai.response.finish_reasons", ["end_turn"]);
  span.setAttribute("gen_ai.request.temperature", 0.5);
  span.setAttribute(
    "gen_ai.system_instructions",
    "You are a creative writing assistant specializing in short stories."
  );
  span.setAttribute(
    "gen_ai.input.messages",
    JSON.stringify([
      {
        role: "user",
        content: "Write a short story about a robot who learns to paint.",
      },
    ])
  );
  span.setAttribute(
    "gen_ai.output.messages",
    JSON.stringify([
      {
        role: "assistant",
        content:
          "**The Last Brushstroke**\n\nUnit-7 had been designed to clean galleries, not create art. But every night, after the visitors left, it would stand before Monet's *Water Lilies* and feel something it couldn't quantify.",
      },
    ])
  );

  span.setStatus({ code: SpanStatusCode.OK });
  span.end();
}

// ── Scenario 6: Deprecated token attributes ────────────────────────

function simulateDeprecatedTokenAttrs() {
  console.log(
    "  Simulating deprecated token attributes (prompt_tokens/completion_tokens)..."
  );
  const span = tracer.startSpan("chat gpt-5.4-mini", {
    kind: SpanKind.CLIENT,
  });

  span.setAttribute("gen_ai.operation.name", "chat");
  span.setAttribute("gen_ai.system", "openai");
  span.setAttribute("gen_ai.request.model", "gpt-5.4-mini");
  span.setAttribute("gen_ai.response.model", "gpt-5.4-mini-20260301");
  // Deprecated attribute names
  span.setAttribute("gen_ai.usage.prompt_tokens", 80);
  span.setAttribute("gen_ai.usage.completion_tokens", 120);
  span.setAttribute(
    "gen_ai.input.messages",
    JSON.stringify([
      { role: "user", content: "What is the capital of France?" },
    ])
  );
  span.setAttribute(
    "gen_ai.output.messages",
    JSON.stringify([
      {
        role: "assistant",
        content:
          "The capital of France is **Paris**. It serves as the country's political, economic, and cultural center.",
      },
    ])
  );

  span.setStatus({ code: SpanStatusCode.OK });
  span.end();
}

// ── Verification ───────────────────────────────────────────────────

async function verifyEndpoints() {
  const endpoint =
    process.env.OTEL_ENDPOINT ?? "http://localhost:3000/api";
  const token = process.env.OTEL_TOKEN;
  const projectId = Number(process.env.PROJECT_ID ?? "1");

  const headers: Record<string, string> = {};
  if (token) headers["Authorization"] = `Bearer ${token}`;

  console.log("\n--- Verifying GenAI endpoints ---");

  // Query trace summaries
  const url = `${endpoint}/otel/genai/traces?project_id=${projectId}&limit=10`;
  console.log(`\nGET ${url}`);
  try {
    const resp = await fetch(url, { headers });
    console.log(`  Status: ${resp.status}`);
    if (resp.ok) {
      const data = (await resp.json()) as any;
      console.log(`  Total traces: ${data.total ?? "?"}`);
      for (const t of data.data ?? []) {
        const tokens =
          (t.total_input_tokens ?? 0) + (t.total_output_tokens ?? 0);
        console.log(
          `    ${t.trace_id.slice(0, 16)}... | ${t.root_span_name} | ` +
            `${t.gen_ai_system ?? "?"} | ${t.gen_ai_model ?? "?"} | ` +
            `${t.span_count} spans | ${tokens} tokens | ` +
            `${t.error_count > 0 ? "ERR" : "OK"}`
        );
      }
    } else {
      console.log(`  Error: ${await resp.text()}`);
    }
  } catch (e) {
    console.log(`  Failed to connect: ${e}`);
  }

  // Filter by Anthropic provider
  const anthropicUrl = `${endpoint}/otel/genai/traces?project_id=${projectId}&limit=10&gen_ai_system=anthropic`;
  console.log(`\nGET ${anthropicUrl}`);
  try {
    const resp = await fetch(anthropicUrl, { headers });
    if (resp.ok) {
      const data = (await resp.json()) as any;
      console.log(`  Anthropic traces: ${data.total ?? "?"}`);
    } else {
      console.log(`  Error: ${resp.status}`);
    }
  } catch (e) {
    console.log(`  Failed: ${e}`);
  }
}

// ── Main ───────────────────────────────────────────────────────────

async function main() {
  const verify = process.argv.includes("--verify");
  const projectId = Number(process.env.PROJECT_ID ?? "1");

  console.log(`\nProject ID: ${projectId}`);
  console.log("\nEmitting GenAI traces...\n");

  // 1. Full agent conversation with tool calls
  simulateAgentConversation();

  // 2. Anthropic chat with extended thinking
  simulateAnthropicChat();

  // 3. RAG pipeline
  simulateRagPipeline();

  // 4. Error trace
  simulateErrorTrace();

  // 5. New gen_ai.provider.name attribute
  simulateNewProviderName();

  // 6. Deprecated token attribute names
  simulateDeprecatedTokenAttrs();

  // Flush spans
  console.log("\nFlushing spans...");
  await shutdownTracing();

  console.log("Done! Emitted 6 traces with various GenAI patterns.");

  if (verify) {
    // Give the backend a moment to process
    await new Promise((r) => setTimeout(r, 2000));
    await verifyEndpoints();
  }
}

main().catch(console.error);
