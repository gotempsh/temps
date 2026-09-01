#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""
Test script that emits OTel GenAI spans to a temps instance.

This script simulates AI agent activity by manually creating spans
with gen_ai.* semantic convention attributes. No actual LLM API keys
are needed — it generates realistic trace structures that exercise
the /otel/genai/traces query endpoints.

Usage:
    # Against local temps instance
    python test_genai_tracing.py

    # Against custom endpoint
    OTEL_ENDPOINT=https://my-temps.example.com python test_genai_tracing.py

    # With auth token
    OTEL_TOKEN=tk_abc123 python test_genai_tracing.py

    # Also test the query endpoints
    python test_genai_tracing.py --verify
"""

import argparse
import json
import os
import random
import time
import uuid

import requests
from opentelemetry import trace
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.trace import StatusCode, SpanKind


def setup_tracer(endpoint: str, token: str | None, project_id: int) -> trace.Tracer:
    """Configure OTel SDK to export to a temps OTLP endpoint."""
    resource = Resource.create({
        "service.name": "test-ai-agent",
        "service.version": "0.1.0",
        "deployment.environment": "development",
    })

    provider = TracerProvider(resource=resource)

    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    headers["X-Temps-Project-Id"] = str(project_id)

    exporter = OTLPSpanExporter(
        endpoint=f"{endpoint}/otel/v1/traces",
        headers=headers,
    )

    provider.add_span_processor(BatchSpanProcessor(
        exporter,
        max_queue_size=100,
        max_export_batch_size=10,
        schedule_delay_millis=1000,
    ))

    trace.set_tracer_provider(provider)
    return trace.get_tracer("test-genai-agent", "0.1.0")


def simulate_chat_completion(
    tracer: trace.Tracer,
    model: str,
    system: str,
    input_tokens: int,
    output_tokens: int,
    duration_ms: int,
    error: bool = False,
    input_messages: str | None = None,
    output_messages: str | None = None,
    system_instructions: str | None = None,
):
    """Simulate a single LLM chat completion span."""
    with tracer.start_as_current_span(
        f"chat {model}",
        kind=SpanKind.CLIENT,
    ) as span:
        # Required GenAI attributes
        span.set_attribute("gen_ai.operation.name", "chat")
        span.set_attribute("gen_ai.system", system)  # deprecated but widely used
        span.set_attribute("gen_ai.request.model", model)
        span.set_attribute("gen_ai.response.model", model)

        # Token usage
        span.set_attribute("gen_ai.usage.input_tokens", input_tokens)
        span.set_attribute("gen_ai.usage.output_tokens", output_tokens)

        # Request parameters
        span.set_attribute("gen_ai.request.temperature", 0.7)
        span.set_attribute("gen_ai.request.max_tokens", 1024)

        # Response metadata
        span.set_attribute("gen_ai.response.id", f"chatcmpl-{uuid.uuid4().hex[:12]}")
        span.set_attribute("gen_ai.response.finish_reasons", ["stop"])

        # Opt-in content (messages) — uses OTel GenAI semantic convention keys
        if system_instructions:
            span.set_attribute("gen_ai.system_instructions", system_instructions)
        if input_messages:
            span.set_attribute("gen_ai.input.messages", input_messages)
        if output_messages:
            span.set_attribute("gen_ai.output.messages", output_messages)

        # Simulate latency
        time.sleep(duration_ms / 1000.0)

        if error:
            span.set_status(StatusCode.ERROR, "Rate limit exceeded")
            span.set_attribute("error.type", "RateLimitError")
        else:
            span.set_status(StatusCode.OK)


def simulate_tool_execution(
    tracer: trace.Tracer,
    tool_name: str,
    duration_ms: int,
    arguments: str | None = None,
    result: str | None = None,
):
    """Simulate a tool execution span."""
    with tracer.start_as_current_span(
        f"execute_tool {tool_name}",
        kind=SpanKind.INTERNAL,
    ) as span:
        span.set_attribute("gen_ai.operation.name", "execute_tool")
        span.set_attribute("gen_ai.tool.name", tool_name)
        span.set_attribute("gen_ai.tool.type", "function")
        span.set_attribute("gen_ai.tool.call.id", f"call_{uuid.uuid4().hex[:12]}")

        if arguments:
            span.set_attribute("gen_ai.tool.call.arguments", arguments)
        if result:
            span.set_attribute("gen_ai.tool.call.result", result)

        time.sleep(duration_ms / 1000.0)
        span.set_status(StatusCode.OK)


def simulate_embeddings(
    tracer: trace.Tracer,
    model: str,
    system: str,
    input_tokens: int,
    duration_ms: int,
):
    """Simulate an embeddings span."""
    with tracer.start_as_current_span(
        f"embeddings {model}",
        kind=SpanKind.CLIENT,
    ) as span:
        span.set_attribute("gen_ai.operation.name", "embeddings")
        span.set_attribute("gen_ai.system", system)
        span.set_attribute("gen_ai.request.model", model)
        span.set_attribute("gen_ai.usage.input_tokens", input_tokens)

        time.sleep(duration_ms / 1000.0)
        span.set_status(StatusCode.OK)


def simulate_agent_conversation(tracer: trace.Tracer):
    """
    Simulate a full agent conversation:
      invoke_agent
        ├── chat gpt-5.4 (initial request, tool_calls finish reason)
        ├── execute_tool search_web
        ├── execute_tool get_weather
        └── chat gpt-5.4 (final response with tool results)
    """
    print("  Simulating agent conversation (OpenAI gpt-5.4)...")
    with tracer.start_as_current_span(
        "invoke_agent research-assistant",
        kind=SpanKind.INTERNAL,
    ) as agent_span:
        agent_span.set_attribute("gen_ai.operation.name", "invoke_agent")
        agent_span.set_attribute("gen_ai.agent.name", "research-assistant")
        agent_span.set_attribute("gen_ai.agent.id", "agent-001")
        agent_span.set_attribute("gen_ai.system", "openai")

        # First LLM call — decides to use tools
        simulate_chat_completion(
            tracer, "gpt-5.4", "openai",
            input_tokens=250, output_tokens=80, duration_ms=50,
            system_instructions="You are a research assistant. Use tools to look up information when needed.",
            input_messages=json.dumps([
                {"role": "user", "content": "What's the weather in San Francisco and find me the latest news about AI?"}
            ]),
            output_messages=json.dumps([
                {"role": "assistant", "content": None, "tool_calls": [
                    {"id": "call_1", "function": {"name": "search_web", "arguments": '{"query": "latest AI news 2026"}'}},
                    {"id": "call_2", "function": {"name": "get_weather", "arguments": '{"city": "San Francisco"}'}}
                ]}
            ]),
        )

        # Tool executions
        simulate_tool_execution(tracer, "search_web", duration_ms=30,
            arguments='{"query": "latest AI news 2026"}',
            result='{"results": [{"title": "OpenAI releases GPT-5", "url": "https://example.com/gpt5"}, {"title": "Anthropic announces Claude 4", "url": "https://example.com/claude4"}]}')
        simulate_tool_execution(tracer, "get_weather", duration_ms=20,
            arguments='{"city": "San Francisco"}',
            result='{"temperature": 62, "condition": "Partly cloudy", "humidity": 72}')

        # Second LLM call — generates final response with tool results
        simulate_chat_completion(
            tracer, "gpt-5.4", "openai",
            input_tokens=600, output_tokens=350, duration_ms=80,
            input_messages=json.dumps([
                {"role": "user", "content": "What's the weather in San Francisco and find me the latest news about AI?"},
                {"role": "tool", "tool_call_id": "call_1", "content": '{"results": [{"title": "OpenAI releases GPT-5"}, {"title": "Anthropic announces Claude 4"}]}'},
                {"role": "tool", "tool_call_id": "call_2", "content": '{"temperature": 62, "condition": "Partly cloudy"}'}
            ]),
            output_messages=json.dumps([
                {"role": "assistant", "content": "Here's what I found:\n\n**Weather in San Francisco:**\nIt's currently 62°F and partly cloudy with 72% humidity.\n\n**Latest AI News:**\n1. OpenAI has released GPT-5 with significant improvements in reasoning\n2. Anthropic announced Claude 4 with enhanced coding capabilities\n\nWould you like more details on any of these topics?"}
            ]),
        )

        agent_span.set_status(StatusCode.OK)


def simulate_anthropic_chat(tracer: trace.Tracer):
    """Simulate an Anthropic chat with extended thinking and tool use (content blocks)."""
    print("  Simulating Anthropic chat (claude-sonnet-4-6-20260311)...")
    simulate_chat_completion(
        tracer, "claude-sonnet-4-6-20260311", "anthropic",
        input_tokens=150, output_tokens=420, duration_ms=60,
        system_instructions="You are a helpful coding assistant with access to tools.",
        input_messages=json.dumps([
            {"role": "user", "content": "What's the current time in Tokyo and write me a haiku about it?"}
        ]),
        output_messages=json.dumps([
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "The user wants two things: 1) current time in Tokyo, and 2) a haiku about it. I should use the get_time tool first to get the accurate time, then compose a haiku based on the result. Tokyo is UTC+9. Let me call the tool first."},
                {"type": "tool_use", "id": "toolu_01ABC", "name": "get_current_time", "input": {"timezone": "Asia/Tokyo"}},
            ]},
            {"role": "tool", "tool_call_id": "toolu_01ABC", "content": "{\"time\": \"2026-03-11T16:53:00+09:00\", \"timezone\": \"JST\"}"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "It's 4:53 PM in Tokyo — late afternoon. The sun would be setting soon. Let me write a haiku that captures the feeling of late afternoon in Tokyo."},
                {"type": "text", "text": "It's currently **4:53 PM JST** in Tokyo.\n\nHere's a haiku:\n\n*Neon lights flicker*\n*Tokyo's afternoon fades*\n*Evening whispers near*"},
            ]},
        ]),
    )


def simulate_rag_pipeline(tracer: trace.Tracer):
    """
    Simulate a RAG pipeline:
      chat (with embeddings + retrieval)
        ├── embeddings text-embedding-4
        └── chat gpt-5.4 (with retrieved context)
    """
    print("  Simulating RAG pipeline (OpenAI)...")
    with tracer.start_as_current_span(
        "rag_query",
        kind=SpanKind.INTERNAL,
    ) as rag_span:
        rag_span.set_attribute("gen_ai.operation.name", "invoke_agent")
        rag_span.set_attribute("gen_ai.agent.name", "rag-pipeline")
        rag_span.set_attribute("gen_ai.system", "openai")

        # Embedding step
        simulate_embeddings(
            tracer, "text-embedding-4", "openai",
            input_tokens=45, duration_ms=15,
        )

        # LLM completion with retrieved context
        simulate_chat_completion(
            tracer, "gpt-5.4", "openai",
            input_tokens=2000, output_tokens=500, duration_ms=100,
            system_instructions="You are a helpful assistant. Answer questions using the provided context documents.",
            input_messages=json.dumps([
                {"role": "user", "content": "What are the best practices for deploying machine learning models in production?"}
            ]),
            output_messages=json.dumps([
                {"role": "assistant", "content": "Based on the retrieved documents, here are the best practices for deploying ML models in production:\n\n1. **Model Versioning** — Use tools like MLflow or DVC to track model versions and their associated training data.\n\n2. **Containerization** — Package models in Docker containers for consistent deployment across environments.\n\n3. **A/B Testing** — Gradually roll out new models using canary deployments to compare performance against the current production model.\n\n4. **Monitoring** — Track model drift, latency, and prediction quality in real-time using tools like Prometheus and Grafana.\n\n5. **Feature Stores** — Use a centralized feature store to ensure consistency between training and serving pipelines."}
            ]),
        )

        rag_span.set_status(StatusCode.OK)


def simulate_error_trace(tracer: trace.Tracer):
    """Simulate a failed LLM call."""
    print("  Simulating error trace (rate limited)...")
    simulate_chat_completion(
        tracer, "gpt-5.4", "openai",
        input_tokens=100, output_tokens=0, duration_ms=10,
        error=True,
        input_messages=json.dumps([
            {"role": "user", "content": "Summarize the top 10 most important events in world history."}
        ]),
    )


def simulate_new_provider_name(tracer: trace.Tracer):
    """
    Simulate using the NEW gen_ai.provider.name attribute
    (instead of deprecated gen_ai.system) to test backward compat.
    """
    print("  Simulating new gen_ai.provider.name attribute (Anthropic)...")
    with tracer.start_as_current_span(
        "chat claude-sonnet-4-6-20260311",
        kind=SpanKind.CLIENT,
    ) as span:
        span.set_attribute("gen_ai.operation.name", "chat")
        # Use the NEW attribute name (not deprecated gen_ai.system)
        span.set_attribute("gen_ai.provider.name", "anthropic")
        span.set_attribute("gen_ai.request.model", "claude-sonnet-4-6-20260311")
        span.set_attribute("gen_ai.response.model", "claude-sonnet-4-6-20260311")
        span.set_attribute("gen_ai.usage.input_tokens", 200)
        span.set_attribute("gen_ai.usage.output_tokens", 300)
        span.set_attribute("gen_ai.response.id", f"msg_{uuid.uuid4().hex[:12]}")
        span.set_attribute("gen_ai.response.finish_reasons", ["end_turn"])
        span.set_attribute("gen_ai.request.temperature", 0.5)
        span.set_attribute("gen_ai.system_instructions", "You are a creative writing assistant specializing in short stories.")
        span.set_attribute("gen_ai.input.messages", json.dumps([
            {"role": "user", "content": "Write a short story about a robot who learns to paint."}
        ]))
        span.set_attribute("gen_ai.output.messages", json.dumps([
            {"role": "assistant", "content": "**The Last Brushstroke**\n\nUnit-7 had been designed to clean galleries, not create art. But every night, after the visitors left and the lights dimmed, it would stand before Monet's *Water Lilies* and feel something it couldn't quantify.\n\nOne evening, it found a forgotten brush and a dried palette. With trembling servos, it touched bristle to canvas.\n\nThe first stroke was mechanical — perfect, precise, utterly lifeless.\n\nBut the hundredth stroke? That one *wobbled*. And in that imperfection, Unit-7 discovered what the humans called beauty."}
        ]))

        time.sleep(0.05)
        span.set_status(StatusCode.OK)


def simulate_deprecated_token_attrs(tracer: trace.Tracer):
    """
    Simulate using deprecated token attribute names
    (prompt_tokens/completion_tokens) to test COALESCE.
    """
    print("  Simulating deprecated token attributes (prompt_tokens/completion_tokens)...")
    with tracer.start_as_current_span(
        "chat gpt-5.4-mini",
        kind=SpanKind.CLIENT,
    ) as span:
        span.set_attribute("gen_ai.operation.name", "chat")
        span.set_attribute("gen_ai.system", "openai")
        span.set_attribute("gen_ai.request.model", "gpt-5.4-mini")
        span.set_attribute("gen_ai.response.model", "gpt-5.4-mini-20260301")
        # Use DEPRECATED attribute names
        span.set_attribute("gen_ai.usage.prompt_tokens", 80)
        span.set_attribute("gen_ai.usage.completion_tokens", 120)
        span.set_attribute("gen_ai.input.messages", json.dumps([
            {"role": "user", "content": "What is the capital of France?"}
        ]))
        span.set_attribute("gen_ai.output.messages", json.dumps([
            {"role": "assistant", "content": "The capital of France is **Paris**. It is the largest city in France and serves as the country's political, economic, and cultural center."}
        ]))

        time.sleep(0.03)
        span.set_status(StatusCode.OK)


def verify_endpoints(endpoint: str, token: str | None, project_id: int):
    """Query the GenAI endpoints to verify traces were ingested."""
    print("\n--- Verifying GenAI endpoints ---")
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"

    # Query GenAI trace summaries
    url = f"{endpoint}/otel/genai/traces"
    params = {"project_id": project_id, "limit": 10}
    print(f"\nGET {url}")
    resp = requests.get(url, params=params, headers=headers)
    print(f"  Status: {resp.status_code}")
    if resp.ok:
        data = resp.json()
        print(f"  Total traces: {data.get('total', '?')}")
        for t in data.get("data", []):
            tokens = (t.get("total_input_tokens") or 0) + (t.get("total_output_tokens") or 0)
            print(
                f"    {t['trace_id'][:16]}... | "
                f"{t['root_span_name']} | "
                f"{t.get('gen_ai_system', '?')} | "
                f"{t.get('gen_ai_model', '?')} | "
                f"{t['span_count']} spans | "
                f"{tokens} tokens | "
                f"{'ERR' if t['error_count'] > 0 else 'OK'}"
            )

            # Drill into the first trace
            if t == data["data"][0]:
                detail_url = f"{endpoint}/otel/genai/traces/{project_id}/{t['trace_id']}"
                print(f"\n  GET {detail_url}")
                detail_resp = requests.get(detail_url, headers=headers)
                if detail_resp.ok:
                    detail = detail_resp.json()
                    print(f"    Spans in trace: {detail['span_count']}")
                    for s in detail["spans"]:
                        indent = "      " if s.get("parent_span_id") else "    "
                        in_tok = s.get("input_tokens", "—")
                        out_tok = s.get("output_tokens", "—")
                        print(
                            f"{indent}{s['name']} | "
                            f"{s.get('gen_ai_operation', '?')} | "
                            f"{s.get('gen_ai_model', '—')} | "
                            f"in={in_tok} out={out_tok} | "
                            f"{s['duration_ms']:.0f}ms | "
                            f"{s['status_code']}"
                        )
                else:
                    print(f"    Error: {detail_resp.status_code} {detail_resp.text[:200]}")
    else:
        print(f"  Error: {resp.text[:300]}")

    # Filter by provider
    print(f"\nGET {url}?gen_ai_system=anthropic")
    resp = requests.get(url, params={**params, "gen_ai_system": "anthropic"}, headers=headers)
    if resp.ok:
        data = resp.json()
        print(f"  Anthropic traces: {data.get('total', '?')}")
    else:
        print(f"  Error: {resp.status_code}")

    # Also test the generic trace attribute filter
    traces_url = f"{endpoint}/otel/trace-summaries"
    print(f"\nGET {traces_url}?attributes=gen_ai.system%3Dopenai")
    resp = requests.get(
        traces_url,
        params={**params, "attributes": "gen_ai.system=openai"},
        headers=headers,
    )
    if resp.ok:
        data = resp.json()
        print(f"  OpenAI traces (via attribute filter): {data.get('total', '?')}")
    else:
        print(f"  Error: {resp.status_code}")


def main():
    parser = argparse.ArgumentParser(description="Test GenAI OTel tracing with temps")
    parser.add_argument(
        "--verify", action="store_true",
        help="Also query the GenAI endpoints to verify ingestion",
    )
    parser.add_argument(
        "--project-id", type=int, default=1,
        help="Project ID to send traces for (default: 1)",
    )
    args = parser.parse_args()

    endpoint = os.environ.get("OTEL_ENDPOINT", "http://localhost:3000/api")
    token = os.environ.get("OTEL_TOKEN")
    project_id = args.project_id

    print(f"Temps endpoint: {endpoint}")
    print(f"Project ID: {project_id}")
    print(f"Auth token: {'set' if token else 'not set'}")
    print()

    tracer = setup_tracer(endpoint, token, project_id)

    print("Emitting GenAI traces...")

    # 1. Full agent conversation with tool calls
    simulate_agent_conversation(tracer)

    # 2. Simple Anthropic chat
    simulate_anthropic_chat(tracer)

    # 3. RAG pipeline
    simulate_rag_pipeline(tracer)

    # 4. Error trace
    simulate_error_trace(tracer)

    # 5. New gen_ai.provider.name attribute
    simulate_new_provider_name(tracer)

    # 6. Deprecated token attribute names
    simulate_deprecated_token_attrs(tracer)

    # Flush spans
    print("\nFlushing spans...")
    provider = trace.get_tracer_provider()
    if hasattr(provider, "force_flush"):
        provider.force_flush(timeout_millis=5000)

    print("Done! Emitted 6 traces with various GenAI patterns.")

    if args.verify:
        # Give the backend a moment to process
        time.sleep(2)
        verify_endpoints(endpoint, token, project_id)


if __name__ == "__main__":
    main()
