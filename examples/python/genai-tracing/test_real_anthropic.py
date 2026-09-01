#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""
Real AI API calls through the Temps AI Gateway with OTel GenAI tracing.

Uses the OpenAI-compatible Temps AI Gateway endpoint to make real LLM calls
(routed to Anthropic with extended thinking), while simultaneously emitting
OTel GenAI spans so the AI Activity dashboard shows real conversations.

Usage:
    # Set your Anthropic API key (for BYOK through the gateway)
    export ANTHROPIC_API_KEY=sk-ant-...

    # Set Temps credentials
    export OTEL_ENDPOINT=http://localhost:8081/api
    export OTEL_TOKEN=tk_...

    python test_real_anthropic.py --project-id 2
"""

import argparse
import json
import os
import time
import uuid

import openai
from opentelemetry import trace
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.trace import StatusCode, SpanKind


def setup_tracer(endpoint: str, token: str | None, project_id: int) -> trace.Tracer:
    """Configure OTel SDK to export to a temps OTLP endpoint."""
    resource = Resource.create({
        "service.name": "ai-gateway-client",
        "service.version": "1.0.0",
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
    return trace.get_tracer("ai-gateway-client", "1.0.0")


def call_via_gateway(
    tracer: trace.Tracer,
    client: openai.OpenAI,
    model: str,
    prompt: str,
    system_prompt: str,
    provider_name: str = "anthropic",
):
    """
    Make a real LLM call through the Temps AI Gateway and record OTel GenAI spans.
    The gateway handles routing to the correct provider.
    """
    print(f"\n  Model: {model}")
    print(f"  Prompt: {prompt[:80]}...")

    with tracer.start_as_current_span(
        f"chat {model}",
        kind=SpanKind.CLIENT,
    ) as chat_span:
        chat_span.set_attribute("gen_ai.operation.name", "chat")
        chat_span.set_attribute("gen_ai.system", provider_name)
        chat_span.set_attribute("gen_ai.request.model", model)
        chat_span.set_attribute("gen_ai.request.max_tokens", 8192)
        chat_span.set_attribute("gen_ai.system_instructions", system_prompt)
        chat_span.set_attribute("gen_ai.input.messages", json.dumps([
            {"role": "user", "content": prompt}
        ]))

        start = time.time()
        try:
            response = client.chat.completions.create(
                model=model,
                max_tokens=8192,
                messages=[
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": prompt},
                ],
            )
            elapsed_ms = (time.time() - start) * 1000

            # Extract response content
            choice = response.choices[0] if response.choices else None
            content = choice.message.content if choice else ""
            finish_reason = choice.finish_reason if choice else "stop"

            # Check for thinking blocks in the response
            # The gateway may return thinking in various formats
            output_messages = []

            # Check if the response has content blocks (Anthropic-style via gateway)
            raw_content = content or ""

            # Build output messages - try to detect thinking blocks
            # The OpenAI-compatible gateway may embed thinking in the content
            if hasattr(choice.message, 'reasoning_content') and choice.message.reasoning_content:
                # Some gateway implementations expose thinking as reasoning_content
                output_messages.append({
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": choice.message.reasoning_content},
                        {"type": "text", "text": raw_content},
                    ]
                })
                print(f"  Thinking: {choice.message.reasoning_content[:100]}...")
            else:
                output_messages.append({
                    "role": "assistant",
                    "content": raw_content,
                })

            print(f"  Response: {raw_content[:120]}...")

            # Set span attributes
            chat_span.set_attribute("gen_ai.response.model", response.model or model)
            chat_span.set_attribute("gen_ai.response.id", response.id or f"chatcmpl-{uuid.uuid4().hex[:12]}")
            chat_span.set_attribute("gen_ai.response.finish_reasons", [finish_reason])
            chat_span.set_attribute("gen_ai.output.messages", json.dumps(output_messages))

            if response.usage:
                chat_span.set_attribute("gen_ai.usage.input_tokens", response.usage.prompt_tokens or 0)
                chat_span.set_attribute("gen_ai.usage.output_tokens", response.usage.completion_tokens or 0)
                print(f"  Tokens: {response.usage.prompt_tokens} in / {response.usage.completion_tokens} out")

            chat_span.set_status(StatusCode.OK)
            print(f"  Latency: {elapsed_ms:.0f}ms")

            return raw_content

        except Exception as e:
            elapsed_ms = (time.time() - start) * 1000
            chat_span.set_status(StatusCode.ERROR, str(e))
            chat_span.set_attribute("error.type", type(e).__name__)
            print(f"  ERROR: {e}")
            return None


def call_agent_conversation(
    tracer: trace.Tracer,
    client: openai.OpenAI,
    model: str,
    provider_name: str = "anthropic",
):
    """Simulate an agent that makes multiple LLM calls (multi-turn via gateway)."""
    print(f"\n  Agent conversation with {model}...")

    with tracer.start_as_current_span(
        "invoke_agent coding-assistant",
        kind=SpanKind.INTERNAL,
    ) as agent_span:
        agent_span.set_attribute("gen_ai.operation.name", "invoke_agent")
        agent_span.set_attribute("gen_ai.agent.name", "coding-assistant")
        agent_span.set_attribute("gen_ai.system", provider_name)

        system_prompt = "You are a senior software engineer. Be concise."

        # First turn - ask a question
        prompt1 = "What's the difference between a mutex and a semaphore? Give me a one-paragraph answer."
        with tracer.start_as_current_span(
            f"chat {model}",
            kind=SpanKind.CLIENT,
        ) as span1:
            span1.set_attribute("gen_ai.operation.name", "chat")
            span1.set_attribute("gen_ai.system", provider_name)
            span1.set_attribute("gen_ai.request.model", model)
            span1.set_attribute("gen_ai.request.max_tokens", 4096)
            span1.set_attribute("gen_ai.system_instructions", system_prompt)
            span1.set_attribute("gen_ai.input.messages", json.dumps([
                {"role": "user", "content": prompt1}
            ]))

            try:
                resp1 = client.chat.completions.create(
                    model=model,
                    max_tokens=4096,
                    messages=[
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": prompt1},
                    ],
                )
                content1 = resp1.choices[0].message.content or ""
                span1.set_attribute("gen_ai.response.model", resp1.model or model)
                span1.set_attribute("gen_ai.response.id", resp1.id or "")
                span1.set_attribute("gen_ai.response.finish_reasons", [resp1.choices[0].finish_reason or "stop"])
                span1.set_attribute("gen_ai.output.messages", json.dumps([
                    {"role": "assistant", "content": content1}
                ]))
                if resp1.usage:
                    span1.set_attribute("gen_ai.usage.input_tokens", resp1.usage.prompt_tokens or 0)
                    span1.set_attribute("gen_ai.usage.output_tokens", resp1.usage.completion_tokens or 0)
                span1.set_status(StatusCode.OK)
                print(f"  Turn 1: {content1[:100]}...")
            except Exception as e:
                span1.set_status(StatusCode.ERROR, str(e))
                print(f"  Turn 1 ERROR: {e}")
                agent_span.set_status(StatusCode.ERROR, str(e))
                return

        # Second turn - follow up
        prompt2 = "Now show me a simple Rust example of a mutex protecting a shared counter."
        with tracer.start_as_current_span(
            f"chat {model}",
            kind=SpanKind.CLIENT,
        ) as span2:
            span2.set_attribute("gen_ai.operation.name", "chat")
            span2.set_attribute("gen_ai.system", provider_name)
            span2.set_attribute("gen_ai.request.model", model)
            span2.set_attribute("gen_ai.request.max_tokens", 4096)
            span2.set_attribute("gen_ai.input.messages", json.dumps([
                {"role": "user", "content": prompt1},
                {"role": "assistant", "content": content1},
                {"role": "user", "content": prompt2},
            ]))

            try:
                resp2 = client.chat.completions.create(
                    model=model,
                    max_tokens=4096,
                    messages=[
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": prompt1},
                        {"role": "assistant", "content": content1},
                        {"role": "user", "content": prompt2},
                    ],
                )
                content2 = resp2.choices[0].message.content or ""
                span2.set_attribute("gen_ai.response.model", resp2.model or model)
                span2.set_attribute("gen_ai.response.id", resp2.id or "")
                span2.set_attribute("gen_ai.response.finish_reasons", [resp2.choices[0].finish_reason or "stop"])
                span2.set_attribute("gen_ai.output.messages", json.dumps([
                    {"role": "assistant", "content": content2}
                ]))
                if resp2.usage:
                    span2.set_attribute("gen_ai.usage.input_tokens", resp2.usage.prompt_tokens or 0)
                    span2.set_attribute("gen_ai.usage.output_tokens", resp2.usage.completion_tokens or 0)
                span2.set_status(StatusCode.OK)
                print(f"  Turn 2: {content2[:100]}...")
            except Exception as e:
                span2.set_status(StatusCode.ERROR, str(e))
                print(f"  Turn 2 ERROR: {e}")

        agent_span.set_status(StatusCode.OK)


def detect_provider(model: str) -> str:
    """Detect provider name from model ID."""
    if "claude" in model or "haiku" in model or "sonnet" in model or "opus" in model:
        return "anthropic"
    elif "gemini" in model:
        return "google"
    elif "grok" in model:
        return "xai"
    return "openai"


def main():
    parser = argparse.ArgumentParser(description="Real AI calls via Temps Gateway with OTel tracing")
    parser.add_argument(
        "--project-id", type=int, default=2,
        help="Project ID to send traces for (default: 2)",
    )
    args = parser.parse_args()

    endpoint = os.environ.get("OTEL_ENDPOINT", "http://localhost:8081/api")
    token = os.environ.get("OTEL_TOKEN")
    project_id = args.project_id

    # Gateway URL is the same base but at /ai/v1
    gateway_base = endpoint.replace("/api", "") + "/api/ai/v1"

    print(f"Temps OTel endpoint: {endpoint}")
    print(f"Temps AI Gateway: {gateway_base}")
    print(f"Project ID: {project_id}")
    print(f"Auth token: {'set' if token else 'not set'}")
    print("Using system-configured provider keys")
    print()

    # Setup OTel tracer for trace export
    tracer = setup_tracer(endpoint, token, project_id)

    # Setup OpenAI client pointing at Temps AI Gateway
    client = openai.OpenAI(
        api_key=token or "dummy",
        base_url=gateway_base,
    )

    # Models to test: cheapest/fastest SOTA from each provider
    models = [
        ("claude-haiku-4-5", "Explain the CAP theorem in distributed systems in 3 sentences."),
        ("gpt-4.1-nano", "What are the SOLID principles in software engineering? One sentence each."),
        ("gemini-2.5-flash", "What is the difference between a process and a thread? Be concise."),
        ("grok-4-1-fast-non-reasoning", "Explain what a load balancer does in 2 sentences."),
    ]

    for i, (model, prompt) in enumerate(models, 1):
        provider = detect_provider(model)
        print(f"\n{'='*60}")
        print(f"{i}. {provider.upper()} — {model}")
        print(f"{'='*60}")
        call_via_gateway(
            tracer, client, model,
            prompt=prompt,
            system_prompt="You are a senior software engineer. Be concise and precise.",
            provider_name=provider,
        )

    # 5. Multi-turn agent conversation (Anthropic)
    print(f"\n{'='*60}")
    print(f"5. ANTHROPIC — Multi-turn agent (claude-haiku-4-5)")
    print(f"{'='*60}")
    call_agent_conversation(tracer, client, "claude-haiku-4-5", provider_name="anthropic")

    # 6. Multi-turn agent conversation (OpenAI)
    print(f"\n{'='*60}")
    print(f"6. OPENAI — Multi-turn agent (gpt-4.1-nano)")
    print(f"{'='*60}")
    call_agent_conversation(tracer, client, "gpt-4.1-nano", provider_name="openai")

    # Flush spans
    print("\n\nFlushing OTel spans...")
    provider = trace.get_tracer_provider()
    if hasattr(provider, "force_flush"):
        provider.force_flush(timeout_millis=10000)

    print(f"\nDone! Emitted 6 real traces across 4 providers via Temps AI Gateway.")
    print("Check the AI Activity tab in your project to see the traces.")


if __name__ == "__main__":
    main()
