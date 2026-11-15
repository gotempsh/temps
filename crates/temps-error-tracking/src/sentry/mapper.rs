// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sentry Event Mapper
//!
//! Converts Sentry SDK events (via relay-event-schema) to our internal error tracking format.

use relay_event_schema::protocol::Event;
use relay_protocol::Annotated;
use serde_json;
use thiserror::Error;

use crate::services::types::{CreateErrorEventData, ExceptionData};

#[derive(Error, Debug)]
pub enum SentryMappingError {
    #[error("Event has no value")]
    NoValue,
    #[error("Validation error: {0}")]
    Validation(String),
}

/// Convert a Sentry event to our internal error data format
pub fn convert_sentry_event_to_error_data(
    event: &Annotated<Event>,
    raw_event: serde_json::Value,
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
) -> Result<CreateErrorEventData, SentryMappingError> {
    let event = event.value().ok_or(SentryMappingError::NoValue)?;

    // SDKs use either a plain message or a structured log entry. Keep the
    // formatted text for display; the original template remains in source data.
    let message = raw_event
        .pointer("/logentry/formatted")
        .and_then(|v| v.as_str())
        .or_else(|| raw_event.get("message").and_then(|v| v.as_str()))
        .or_else(|| {
            raw_event
                .pointer("/logentry/message")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            raw_event
                .pointer("/message/formatted")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            raw_event
                .pointer("/message/message")
                .and_then(|v| v.as_str())
        })
        .unwrap_or("Error")
        .to_string();

    // Extract all exceptions with their stack traces
    let mut exceptions_list: Vec<ExceptionData> = Vec::new();

    if let Some(exceptions) = event.exceptions.value() {
        if let Some(values_vec) = exceptions.values.value() {
            for (index, annotated_exception) in values_vec.iter().enumerate() {
                if let Some(exc) = annotated_exception.value() {
                    let exception_type = exc
                        .ty
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Error".to_string());

                    let exception_value = exc.value.as_str().map(|s| s.to_string());

                    let wire_exception = raw_event
                        .get("exception")
                        .and_then(|value| {
                            value
                                .as_array()
                                .or_else(|| value.get("values").and_then(|v| v.as_array()))
                        })
                        .and_then(|values| values.get(index));
                    // Extract stack trace for this exception
                    let stack_trace = wire_exception
                        .and_then(|value| value.get("stacktrace"))
                        .filter(|v| v.is_object())
                        .cloned()
                        .or_else(|| exc.stacktrace.value().and_then(convert_stacktrace_to_json));

                    // Extract mechanism if available
                    let mechanism = wire_exception
                        .and_then(|value| value.get("mechanism"))
                        .filter(|v| v.is_object())
                        .cloned();

                    let module = exc.module.as_str().map(|s| s.to_string());
                    let thread_id = exc.thread_id.value().map(|id| id.to_string());

                    exceptions_list.push(ExceptionData {
                        exception_type,
                        exception_value,
                        stack_trace,
                        mechanism,
                        module,
                        thread_id,
                    });
                }
            }
        }
    }

    // If no exceptions found but event has a stacktrace at the top level, create a generic exception
    if exceptions_list.is_empty() {
        let stack_trace = raw_event
            .get("stacktrace")
            .filter(|v| v.is_object())
            .cloned()
            .or_else(|| {
                event
                    .stacktrace
                    .value()
                    .and_then(convert_stacktrace_to_json)
            });

        exceptions_list.push(ExceptionData {
            exception_type: if raw_event.get("type").and_then(|v| v.as_str()) == Some("csp") {
                "CSPViolation"
            } else {
                "Message"
            }
            .into(),
            exception_value: Some(message.clone()),
            stack_trace,
            mechanism: None,
            module: None,
            thread_id: None,
        });
    }

    // Some native SDKs attach the capture stack to a thread. Match an
    // explicit exception thread ID, or the unique current/crashed thread for
    // the primary exception. Never pick an arbitrary thread from a crash.
    let primary = exceptions_list.len().saturating_sub(1);
    for (index, exception) in exceptions_list.iter_mut().enumerate() {
        if exception.stack_trace.is_none() {
            exception.stack_trace =
                thread_stack(&raw_event, exception.thread_id.as_deref(), index == primary);
        }
    }

    // Sentry exception chains are ordered oldest to most recent.
    let (exception_type, exception_value, stack_trace) = if let Some(first) = exceptions_list.last()
    {
        (
            Some(first.exception_type.clone()),
            first.exception_value.clone(),
            first.stack_trace.clone(),
        )
    } else {
        (Some(message.clone()), None, None)
    };

    // Preserve typed breadcrumb data, nulls, timestamps and unknown fields.
    // SDKs use both a direct array and the legacy { values: [...] } wrapper.
    let json_breadcrumbs = raw_event
        .get("breadcrumbs")
        .and_then(|value| {
            value
                .as_array()
                .map(|_| value)
                .or_else(|| value.get("values").filter(|v| v.is_array()))
        })
        .cloned();

    // Extract user information
    let (user_id, user_email, user_username, user_ip) = if let Some(user) = event.user.value() {
        (
            user.id.as_str().map(|s| s.to_string()),
            user.email.as_str().map(|s| s.to_string()),
            user.username.as_str().map(|s| s.to_string()),
            user.ip_address.value().map(|ip| ip.to_string()),
        )
    } else {
        (None, None, None, None)
    };

    let text = |path: &str| {
        raw_event
            .pointer(path)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let field = |path: &str| raw_event.pointer(path).filter(|v| !v.is_null()).cloned();
    let integer = |path: &str| raw_event.pointer(path).and_then(|v| v.as_i64());
    let header = |name: &str| request_header(raw_event.pointer("/request/headers"), name);
    let timestamp = |path: &str| {
        text(path)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    let native_extra = if raw_event.get("tags").is_some() || raw_event.get("extra").is_some() {
        Some(serde_json::json!({"tags": raw_event.get("tags"), "extra": raw_event.get("extra")}))
    } else {
        None
    };

    Ok(CreateErrorEventData {
        source: Some("sentry".to_string()),
        raw_sentry_event: Some(raw_event.clone()),
        exceptions: exceptions_list,
        exception_type,
        exception_value,
        stack_trace,
        url: text("/request/url"),
        user_agent: header("user-agent"),
        referrer: header("referer"),
        method: text("/request/method"),
        headers: field("/request/headers"),
        user_id: user_id.or_else(|| {
            raw_event
                .pointer("/user/id")
                .filter(|v| v.is_number())
                .map(ToString::to_string)
        }),
        user_email,
        user_username,
        user_ip_address: user_ip,
        user_segment: text("/user/segment"),
        session_id: text("/contexts/replay/replay_id")
            .or_else(|| text("/replay_id"))
            .filter(|id| super::logs::valid_id(id, 32))
            .map(|id| {
                format!(
                    "sentry-{project_id}-{}-{}-{}",
                    environment_id.unwrap_or(0),
                    deployment_id.unwrap_or(0),
                    id.to_ascii_lowercase()
                )
            }),
        user_context: field("/user"),
        browser: text("/contexts/browser/name"),
        browser_version: text("/contexts/browser/version"),
        operating_system: text("/contexts/os/name"),
        operating_system_version: text("/contexts/os/version"),
        device_type: text("/contexts/device/family").or_else(|| text("/contexts/device/model")),
        screen_width: integer("/contexts/device/screen_width_pixels")
            .and_then(|v| v.try_into().ok()),
        screen_height: integer("/contexts/device/screen_height_pixels")
            .and_then(|v| v.try_into().ok()),
        viewport_width: None,
        viewport_height: None,
        request_context: field("/request"),
        extra_context: native_extra,
        release_version: event.release.as_str().map(|r| r.to_string()),
        build_number: text("/dist"),
        server_name: event.server_name.as_str().map(|s| s.to_string()),
        environment: event.environment.as_str().map(|e| e.to_string()),
        sdk_name: event
            .client_sdk
            .value()
            .and_then(|s| s.name.as_str().map(|n| n.to_string())),
        sdk_version: event
            .client_sdk
            .value()
            .and_then(|s| s.version.as_str().map(|v| v.to_string())),
        sdk_integrations: field("/sdk/integrations"),
        platform: event.platform.as_str().map(|p| p.to_string()),
        transaction_name: event.transaction.as_str().map(|t| t.to_string()),
        breadcrumbs: json_breadcrumbs,
        request_cookies: field("/request/cookies"),
        request_query_string: field("/request/query_string"),
        request_data: field("/request/data"),
        contexts: field("/contexts"),
        os_name: text("/contexts/os/name"),
        os_version: text("/contexts/os/version"),
        os_build: text("/contexts/os/build"),
        os_kernel_version: text("/contexts/os/kernel_version"),
        device_arch: text("/contexts/device/arch"),
        device_processor_count: integer("/contexts/device/processor_count")
            .and_then(|v| v.try_into().ok()),
        device_processor_frequency: integer("/contexts/device/processor_frequency")
            .and_then(|v| v.try_into().ok()),
        device_memory_size: integer("/contexts/device/memory_size"),
        device_free_memory: integer("/contexts/device/free_memory"),
        device_boot_time: timestamp("/contexts/device/boot_time"),
        runtime_name: text("/contexts/runtime/name"),
        runtime_version: text("/contexts/runtime/version"),
        app_start_time: timestamp("/contexts/app/app_start_time"),
        app_memory: integer("/contexts/app/app_memory"),
        locale: text("/contexts/device/locale").or_else(|| text("/contexts/culture/locale")),
        timezone: text("/contexts/culture/timezone"),
        project_id,
        environment_id,
        deployment_id,
        visitor_id: None,
        ip_geolocation_id: None,
    })
}

/// Validate grouping input before any native writes. The body framing limit
/// remains the primary bound for all other preserved event metadata.
pub fn validate_event_metadata(raw: &serde_json::Value) -> Result<(), SentryMappingError> {
    if let Some(value) = raw.get("fingerprint").filter(|v| !v.is_null()) {
        let values = value.as_array().ok_or_else(|| {
            SentryMappingError::Validation("fingerprint must be a string array".into())
        })?;
        if values.len() > 128
            || values.iter().any(|v| {
                v.as_str()
                    .is_none_or(|s| s.len() > 1024 || s.contains('\0'))
            })
        {
            return Err(SentryMappingError::Validation(
                "fingerprint exceeds component/type limits".into(),
            ));
        }
    }
    Ok(())
}

fn thread_stack(
    raw: &serde_json::Value,
    id: Option<&str>,
    primary: bool,
) -> Option<serde_json::Value> {
    let threads = raw.get("threads").and_then(|v| {
        v.as_array()
            .or_else(|| v.get("values").and_then(|v| v.as_array()))
    })?;
    let mut matches = threads.iter().filter(|thread| {
        if let Some(id) = id {
            thread.get("id").is_some_and(|value| {
                value.as_str() == Some(id)
                    || value.as_number().is_some_and(|number| {
                        id.parse::<serde_json::Number>()
                            .is_ok_and(|id| number == &id)
                    })
            })
        } else {
            primary
                && (thread.get("current").and_then(|v| v.as_bool()) == Some(true)
                    || thread.get("crashed").and_then(|v| v.as_bool()) == Some(true))
        }
    });
    let thread = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    thread.get("stacktrace").filter(|v| v.is_object()).cloned()
}

fn request_header(headers: Option<&serde_json::Value>, name: &str) -> Option<String> {
    match headers? {
        serde_json::Value::Object(values) => values
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
            .map(str::to_owned),
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(|v| v.as_array())
            .find(|pair| {
                pair.first()
                    .and_then(|v| v.as_str())
                    .is_some_and(|key| key.eq_ignore_ascii_case(name))
            })
            .and_then(|pair| pair.get(1))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        _ => None,
    }
}

/// Convert a Relay stack trace to JSON
pub fn convert_stacktrace_to_json(
    stacktrace: &relay_event_schema::protocol::Stacktrace,
) -> Option<serde_json::Value> {
    stacktrace.frames.value().map(|frames| {
        let frame_array: Vec<serde_json::Value> = frames
            .iter()
            .filter_map(|annotated_frame| {
                annotated_frame.value().map(|frame| {
                    let mut obj = serde_json::Map::new();

                    // Filename is NativeImagePath - use as_str() to get inner string
                    if let Some(filename) = frame.filename.value() {
                        obj.insert(
                            "filename".to_string(),
                            serde_json::Value::String(filename.as_str().to_string()),
                        );
                    }

                    // abs_path is the full URL/path (critical for server-side source map matching)
                    if let Some(abs_path) = frame.abs_path.value() {
                        obj.insert(
                            "abs_path".to_string(),
                            serde_json::Value::String(abs_path.as_str().to_string()),
                        );
                    }
                    if let Some(function) = frame.function.as_str() {
                        obj.insert(
                            "function".to_string(),
                            serde_json::Value::String(function.to_string()),
                        );
                    }
                    if let Some(module) = frame.module.as_str() {
                        obj.insert(
                            "module".to_string(),
                            serde_json::Value::String(module.to_string()),
                        );
                    }
                    if let Some(lineno) = frame.lineno.value() {
                        obj.insert(
                            "lineno".to_string(),
                            serde_json::Value::Number((*lineno).into()),
                        );
                    }
                    if let Some(colno) = frame.colno.value() {
                        obj.insert(
                            "colno".to_string(),
                            serde_json::Value::Number((*colno).into()),
                        );
                    }
                    if let Some(in_app) = frame.in_app.value() {
                        obj.insert("in_app".to_string(), serde_json::Value::Bool(*in_app));
                    }
                    if let Some(pre_context) = frame.pre_context.value() {
                        let pre_array: Vec<serde_json::Value> = pre_context
                            .iter()
                            .filter_map(|s| {
                                s.as_str().map(|v| serde_json::Value::String(v.to_string()))
                            })
                            .collect();
                        obj.insert(
                            "pre_context".to_string(),
                            serde_json::Value::Array(pre_array),
                        );
                    }
                    if let Some(context_line) = frame.context_line.as_str() {
                        obj.insert(
                            "context_line".to_string(),
                            serde_json::Value::String(context_line.to_string()),
                        );
                    }
                    if let Some(post_context) = frame.post_context.value() {
                        let post_array: Vec<serde_json::Value> = post_context
                            .iter()
                            .filter_map(|s| {
                                s.as_str().map(|v| serde_json::Value::String(v.to_string()))
                            })
                            .collect();
                        obj.insert(
                            "post_context".to_string(),
                            serde_json::Value::Array(post_array),
                        );
                    }

                    serde_json::Value::Object(obj)
                })
            })
            .collect();

        serde_json::json!({ "frames": frame_array })
    })
}

#[cfg(test)]
mod conformance_tests {
    use super::*;
    use relay_protocol::FromValue;
    use serde_json::json;

    #[test]
    fn sdk_metadata_populates_native_fields_and_preserves_source() {
        let raw = json!({"event_id":"11111111111111111111111111111111","logentry":{"formatted":"Synthetic item 42","message":"Synthetic item %s","params":[42]},"exception":{"values":[{"type":"InnerError","value":"inner"},{"type":"OuterError","value":"outer","stacktrace":{"frames":[{"filename":"synthetic.py","function":"run","lineno":7,"in_app":true,"vars":{"synthetic":null}}]},"mechanism":{"type":"generic","handled":false,"data":{"synthetic":true}}}]},"user":{"id":"synthetic-user","email":"synthetic@example.com","segment":"sample","custom":{"nullable":null}},"request":{"url":"https://synthetic.example/path","method":"POST","headers":[["User-Agent","synthetic-agent"],["Referer","https://synthetic.example/"]],"query_string":[["sample","yes"]],"cookies":"sample=synthetic","data":{"synthetic":true}},"tags":{"sample":"yes"},"extra":{"nullable":null,"integer":9007199254740993_u64},"contexts":{"browser":{"name":"SyntheticBrowser","version":"1"},"os":{"name":"SyntheticOS","version":"2","build":"sample","kernel_version":"3"},"device":{"family":"sample","arch":"arm64","processor_count":4,"screen_width_pixels":390,"screen_height_pixels":844,"memory_size":4096},"runtime":{"name":"synthetic-runtime","version":"4"},"culture":{"locale":"en","timezone":"UTC"},"custom":{"null":null,"flag":true}},"sdk":{"name":"synthetic-sdk","version":"1","integrations":["sample"]},"release":"synthetic-release","dist":"sample"});
        let event = Event::from_value(raw.clone().into());
        let data =
            convert_sentry_event_to_error_data(&event, raw.clone(), 1, Some(2), Some(3)).unwrap();
        assert_eq!(data.exception_type.as_deref(), Some("OuterError"));
        assert_eq!(data.exceptions.len(), 2);
        assert_eq!(
            data.exceptions[1].mechanism.as_ref().unwrap()["data"]["synthetic"],
            true
        );
        assert_eq!(data.user_id.as_deref(), Some("synthetic-user"));
        assert_eq!(data.user_segment.as_deref(), Some("sample"));
        assert_eq!(data.user_agent.as_deref(), Some("synthetic-agent"));
        assert_eq!(data.browser.as_deref(), Some("SyntheticBrowser"));
        assert_eq!(data.os_kernel_version.as_deref(), Some("3"));
        assert_eq!(data.device_processor_count, Some(4));
        assert_eq!(data.screen_width, Some(390));
        assert_eq!(data.runtime_name.as_deref(), Some("synthetic-runtime"));
        assert_eq!(data.sdk_integrations, Some(json!(["sample"])));
        assert_eq!(data.contexts.as_ref().unwrap()["custom"]["flag"], true);
        assert_eq!(
            data.extra_context.as_ref().unwrap()["tags"]["sample"],
            "yes"
        );
        assert_eq!(data.request_data, Some(json!({"synthetic":true})));
        assert_eq!(data.raw_sentry_event, Some(raw));
        assert_eq!(
            (data.project_id, data.environment_id, data.deployment_id),
            (1, Some(2), Some(3))
        );
    }

    #[test]
    fn messages_use_formatted_text_and_request_header_objects_are_supported() {
        let raw = json!({"event_id":"11111111111111111111111111111111","logentry":{"formatted":"Synthetic display","message":"Synthetic template"},"request":{"headers":{"user-AGENT":"synthetic-agent"}}});
        let event = Event::from_value(raw.clone().into());
        let data = convert_sentry_event_to_error_data(&event, raw, 1, None, None).unwrap();
        assert_eq!(data.exception_type.as_deref(), Some("Message"));
        assert_eq!(data.exception_value.as_deref(), Some("Synthetic display"));
        assert_eq!(data.user_agent.as_deref(), Some("synthetic-agent"));
        let raw = json!({"event_id":"11111111111111111111111111111111", "user":{"id":42}, "breadcrumbs":{"values":[{"timestamp":1790000000.25,"data":{"nullable":null,"values":[1,null,2],"large":9007199254740993_u64}}]}});
        let event = Event::from_value(raw.clone().into());
        let data = convert_sentry_event_to_error_data(&event, raw.clone(), 1, None, None).unwrap();
        assert_eq!(
            data.breadcrumbs,
            raw.pointer("/breadcrumbs/values").cloned()
        );
        assert_eq!(data.user_id.as_deref(), Some("42"));
    }

    #[test]
    fn native_thread_stacks_match_ids_or_unique_primary_thread() {
        let raw = json!({"event_id":"11111111111111111111111111111111","exception":{"values":[{"type":"SyntheticError","value":"Synthetic failure"}]},"threads":{"values":[{"id":42,"current":true,"stacktrace":{"frames":[{"filename":"synthetic.rs","function":"run","lineno":7}]}}]}});
        let event = Event::from_value(raw.clone().into());
        let data = convert_sentry_event_to_error_data(&event, raw.clone(), 1, None, None).unwrap();
        assert_eq!(
            data.stack_trace.as_ref().unwrap()["frames"][0]["function"],
            "run"
        );
        assert_eq!(data.exceptions[0].stack_trace, data.stack_trace);
        assert_eq!(data.raw_sentry_event, Some(raw.clone()));
        let legacy = json!({"event_id":"11111111111111111111111111111111","exception":[{"type":"SyntheticError","value":"Synthetic failure","stacktrace":{"frames":[{"filename":"synthetic.go","in_app":true,"vars":{"nullable":null}}]},"mechanism":{"type":"generic","handled":true,"data":{"nullable":null}}}]});
        let event = Event::from_value(legacy.clone().into());
        let legacy_data =
            convert_sentry_event_to_error_data(&event, legacy, 1, None, None).unwrap();
        assert_eq!(
            legacy_data.stack_trace.as_ref().unwrap()["frames"][0]["in_app"],
            true
        );
        assert!(
            legacy_data.exceptions[0].mechanism.as_ref().unwrap()["data"]
                .get("nullable")
                .is_some()
        );
        let mut ambiguous = raw.clone();
        ambiguous["threads"]["values"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":43,"crashed":true,"stacktrace":{"frames":[]}}));
        assert!(thread_stack(&ambiguous, None, true).is_none());
        assert!(thread_stack(&raw, None, false).is_none());
        assert!(thread_stack(&ambiguous, Some("42"), false).is_some());
        assert!(thread_stack(&ambiguous, Some("99"), true).is_none());
    }

    #[test]
    fn fingerprints_are_bounded_and_invalid_types_are_rejected_without_echoing_values() {
        for raw in [
            json!({}),
            json!({"fingerprint":null}),
            json!({"fingerprint":[]}),
            json!({"fingerprint":["{{ default }}","sample"]}),
        ] {
            assert!(validate_event_metadata(&raw).is_ok());
        }
        for raw in [
            json!({"fingerprint":"private-value"}),
            json!({"fingerprint":[42]}),
            json!({"fingerprint":vec!["sample";129]}),
        ] {
            let error = validate_event_metadata(&raw).unwrap_err().to_string();
            assert!(!error.contains("private-value"));
        }
        assert!(validate_event_metadata(&json!({"fingerprint":["x".repeat(1025)]})).is_err());
    }
}
