#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""
Real AI API calls through the Temps AI Gateway with AUTOMATIC OTel GenAI tracing.

Unlike test_real_anthropic.py which manually sets gen_ai.* span attributes,
this script uses the `opentelemetry-instrumentation-openai-v2` library to
automatically instrument OpenAI client calls. The instrumentor patches the
OpenAI SDK and emits gen_ai.* spans (model, tokens, messages, etc.) with
zero manual attribute setting.

This works with the Temps AI Gateway because it exposes an OpenAI-compatible
API — so the OpenAI instrumentor captures all calls automatically.

Install:
    pip install opentelemetry-instrumentation-openai-v2

Usage:
    export OTEL_ENDPOINT=http://localhost:8081/api
    export OTEL_TOKEN=tk_...

    python test_auto_instrumented.py --project-id 2
"""

import argparse
import os
import time

import openai
from opentelemetry import trace
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.openai_v2 import OpenAIInstrumentor


def setup_tracing(endpoint: str, token: str | None, project_id: int):
    """Configure OTel SDK with OTLP exporter and enable OpenAI auto-instrumentation."""
    resource = Resource.create({
        "service.name": "ai-gateway-client-auto",
        "service.version": "1.0.0",
        "deployment.environment": "development",
    })

    provider = TracerProvider(resource=resource)

    headers = {"X-Temps-Project-Id": str(project_id)}
    if token:
        headers["Authorization"] = f"Bearer {token}"

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

    # This single line auto-instruments ALL OpenAI client calls
    # It patches openai.ChatCompletion.create/acreate to emit gen_ai.* spans
    OpenAIInstrumentor().instrument()

    print("Auto-instrumentation enabled: OpenAI SDK calls will emit gen_ai.* spans automatically")
    return provider


def main():
    parser = argparse.ArgumentParser(description="Auto-instrumented AI calls via Temps Gateway")
    parser.add_argument(
        "--project-id", type=int, default=2,
        help="Project ID to send traces for (default: 2)",
    )
    args = parser.parse_args()

    endpoint = os.environ.get("OTEL_ENDPOINT", "http://localhost:8081/api")
    token = os.environ.get("OTEL_TOKEN")
    project_id = args.project_id

    gateway_base = endpoint.replace("/api", "") + "/api/ai/v1"

    print(f"Temps OTel endpoint: {endpoint}")
    print(f"Temps AI Gateway: {gateway_base}")
    print(f"Project ID: {project_id}")
    print(f"Auth token: {'set' if token else 'not set'}")
    print()

    provider = setup_tracing(endpoint, token, project_id)

    # OpenAI client pointing at Temps AI Gateway
    client = openai.OpenAI(
        api_key=token or "dummy",
        base_url=gateway_base,
    )

    # All calls below are automatically instrumented — no manual span creation needed!

    # 1. Simple chat completion (Anthropic via gateway)
    print(f"\n{'='*60}")
    print("1. ANTHROPIC — claude-haiku-4-5 (auto-instrumented)")
    print(f"{'='*60}")
    start = time.time()
    resp = client.chat.completions.create(
        model="claude-haiku-4-5",
        max_tokens=1024,
        messages=[
            {"role": "system", "content": "You are a senior software engineer. Be concise."},
            {"role": "user", "content": "Explain the CAP theorem in distributed systems in 3 sentences."},
        ],
    )
    elapsed = (time.time() - start) * 1000
    print(f"  Response: {resp.choices[0].message.content[:120]}...")
    if resp.usage:
        print(f"  Tokens: {resp.usage.prompt_tokens} in / {resp.usage.completion_tokens} out")
    print(f"  Latency: {elapsed:.0f}ms")

    # 2. OpenAI model
    print(f"\n{'='*60}")
    print("2. OPENAI — gpt-4.1-nano (auto-instrumented)")
    print(f"{'='*60}")
    start = time.time()
    resp = client.chat.completions.create(
        model="gpt-4.1-nano",
        max_tokens=1024,
        messages=[
            {"role": "system", "content": "You are a senior software engineer. Be concise."},
            {"role": "user", "content": "What are the SOLID principles? One sentence each."},
        ],
    )
    elapsed = (time.time() - start) * 1000
    print(f"  Response: {resp.choices[0].message.content[:120]}...")
    if resp.usage:
        print(f"  Tokens: {resp.usage.prompt_tokens} in / {resp.usage.completion_tokens} out")
    print(f"  Latency: {elapsed:.0f}ms")

    # 3. Google Gemini model
    print(f"\n{'='*60}")
    print("3. GOOGLE — gemini-2.5-flash (auto-instrumented)")
    print(f"{'='*60}")
    start = time.time()
    resp = client.chat.completions.create(
        model="gemini-2.5-flash",
        max_tokens=1024,
        messages=[
            {"role": "system", "content": "You are a senior software engineer. Be concise."},
            {"role": "user", "content": "What is the difference between a process and a thread?"},
        ],
    )
    elapsed = (time.time() - start) * 1000
    print(f"  Response: {resp.choices[0].message.content[:120]}...")
    if resp.usage:
        print(f"  Tokens: {resp.usage.prompt_tokens} in / {resp.usage.completion_tokens} out")
    print(f"  Latency: {elapsed:.0f}ms")

    # 4. xAI Grok model
    print(f"\n{'='*60}")
    print("4. XAI — grok-4-1-fast-non-reasoning (auto-instrumented)")
    print(f"{'='*60}")
    start = time.time()
    resp = client.chat.completions.create(
        model="grok-4-1-fast-non-reasoning",
        max_tokens=1024,
        messages=[
            {"role": "system", "content": "You are a senior software engineer. Be concise."},
            {"role": "user", "content": "Explain what a load balancer does in 2 sentences."},
        ],
    )
    elapsed = (time.time() - start) * 1000
    print(f"  Response: {resp.choices[0].message.content[:120]}...")
    if resp.usage:
        print(f"  Tokens: {resp.usage.prompt_tokens} in / {resp.usage.completion_tokens} out")
    print(f"  Latency: {elapsed:.0f}ms")

    # 5. Multi-turn conversation (auto-instrumented — each create() call gets its own span)
    print(f"\n{'='*60}")
    print("5. Multi-turn conversation (auto-instrumented)")
    print(f"{'='*60}")

    messages = [
        {"role": "system", "content": "You are a senior software engineer. Be concise."},
        {"role": "user", "content": "What's the difference between a mutex and a semaphore? One paragraph."},
    ]

    resp1 = client.chat.completions.create(
        model="claude-haiku-4-5",
        max_tokens=2048,
        messages=messages,
    )
    content1 = resp1.choices[0].message.content or ""
    print(f"  Turn 1: {content1[:100]}...")

    messages.append({"role": "assistant", "content": content1})
    messages.append({"role": "user", "content": "Now show me a simple Rust example of a mutex protecting a shared counter."})

    resp2 = client.chat.completions.create(
        model="claude-haiku-4-5",
        max_tokens=2048,
        messages=messages,
    )
    content2 = resp2.choices[0].message.content or ""
    print(f"  Turn 2: {content2[:100]}...")

    # Flush spans
    print("\n\nFlushing OTel spans...")
    if hasattr(provider, "force_flush"):
        provider.force_flush(timeout_millis=10000)

    print(f"\nDone! All 6 API calls were auto-instrumented with gen_ai.* attributes.")
    print("Check the AI Activity tab in your project to see the traces.")
    print("\nKey difference from test_real_anthropic.py:")
    print("  - ZERO manual span creation or gen_ai.* attribute setting")
    print("  - OpenAIInstrumentor().instrument() does everything automatically")


if __name__ == "__main__":
    main()
