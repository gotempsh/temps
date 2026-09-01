// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "proto";
    let mut config = prost_build::Config::new();

    // Ubuntu 22.04's protoc requires this flag for the `optional` fields in
    // the OpenTelemetry metrics schema. Newer protoc releases accept them
    // without it, which previously hid this release-build-only failure.
    config
        .btree_map(["."])
        .protoc_arg("--experimental_allow_proto3_optional");

    config.compile_protos(
        &[
            "proto/opentelemetry/proto/common/v1/common.proto",
            "proto/opentelemetry/proto/resource/v1/resource.proto",
            "proto/opentelemetry/proto/metrics/v1/metrics.proto",
            "proto/opentelemetry/proto/trace/v1/trace.proto",
            "proto/opentelemetry/proto/logs/v1/logs.proto",
            "proto/opentelemetry/proto/collector/metrics/v1/metrics_service.proto",
            "proto/opentelemetry/proto/collector/trace/v1/trace_service.proto",
            "proto/opentelemetry/proto/collector/logs/v1/logs_service.proto",
        ],
        &[proto_root],
    )?;

    println!("cargo:rerun-if-changed=proto/");
    Ok(())
}
