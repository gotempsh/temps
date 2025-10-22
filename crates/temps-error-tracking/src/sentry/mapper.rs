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

    // Extract message - for now, just use a default
    let message: String = event
        .logentry
        .value()
        .and_then(|logentry| logentry.message.value().map(|msg| msg.as_ref().to_string()))
        .unwrap_or_else(|| "Error".to_string());

    // Extract all exceptions with their stack traces
    let mut exceptions_list: Vec<ExceptionData> = Vec::new();

    if let Some(exceptions) = event.exceptions.value() {
        if let Some(values_vec) = exceptions.values.value() {
            for annotated_exception in values_vec.iter() {
                if let Some(exc) = annotated_exception.value() {
                    let exception_type = exc
                        .ty
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Error".to_string());

                    let exception_value = exc.value.as_str().map(|s| s.to_string());

                    // Extract stack trace for this exception
                    let stack_trace = exc.stacktrace.value().and_then(convert_stacktrace_to_json);

                    // Extract mechanism if available
                    let mechanism = exc.mechanism.value().map(|mech| {
                        let mut obj = serde_json::Map::new();
                        if let Some(ty) = mech.ty.as_str() {
                            obj.insert(
                                "type".to_string(),
                                serde_json::Value::String(ty.to_string()),
                            );
                        }
                        if let Some(handled) = mech.handled.value() {
                            obj.insert("handled".to_string(), serde_json::Value::Bool(*handled));
                        }
                        if let Some(synthetic) = mech.synthetic.value() {
                            obj.insert(
                                "synthetic".to_string(),
                                serde_json::Value::Bool(*synthetic),
                            );
                        }
                        serde_json::Value::Object(obj)
                    });

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
        let stack_trace = event
            .stacktrace
            .value()
            .and_then(convert_stacktrace_to_json);

        exceptions_list.push(ExceptionData {
            exception_type: message.clone(),
            exception_value: None,
            stack_trace,
            mechanism: None,
            module: None,
            thread_id: None,
        });
    }

    // For backward compatibility, extract first exception data
    let (exception_type, exception_value, stack_trace) =
        if let Some(first) = exceptions_list.first() {
            (
                Some(first.exception_type.clone()),
                first.exception_value.clone(),
                first.stack_trace.clone(),
            )
        } else {
            (Some(message.clone()), None, None)
        };

    // Extract breadcrumbs
    let mut json_breadcrumbs: Option<serde_json::Value> = None;
    if let Some(breadcrumbs) = event.breadcrumbs.value() {
        if let Some(values) = breadcrumbs.values.value() {
            let items: Vec<serde_json::Value> = values
                .iter()
                .filter_map(|annotated_breadcrumb| {
                    annotated_breadcrumb.value().map(|breadcrumb| {
                        let mut obj = serde_json::Map::new();

                        if let Some(ty) = breadcrumb.ty.as_str() {
                            obj.insert(
                                "type".to_string(),
                                serde_json::Value::String(ty.to_string()),
                            );
                        }
                        if let Some(category) = breadcrumb.category.as_str() {
                            obj.insert(
                                "category".to_string(),
                                serde_json::Value::String(category.to_string()),
                            );
                        }
                        if let Some(message) = breadcrumb.message.as_str() {
                            obj.insert(
                                "message".to_string(),
                                serde_json::Value::String(message.to_string()),
                            );
                        }
                        if let Some(level) = breadcrumb.level.value() {
                            obj.insert(
                                "level".to_string(),
                                serde_json::Value::String(level.to_string()),
                            );
                        }
                        if let Some(timestamp) = breadcrumb.timestamp.value() {
                            let dt = timestamp.into_inner();
                            obj.insert(
                                "timestamp".to_string(),
                                serde_json::Value::String(dt.to_rfc3339()),
                            );
                        }
                        if let Some(data) = breadcrumb.data.value() {
                            let map: serde_json::Map<String, serde_json::Value> = data
                                .iter()
                                .filter_map(|(k, v)| {
                                    v.value().and_then(|value| {
                                        relay_value_to_json(value)
                                            .map(|json_val| (k.clone(), json_val))
                                    })
                                })
                                .collect();
                            obj.insert("data".to_string(), serde_json::Value::Object(map));
                        }

                        serde_json::Value::Object(obj)
                    })
                })
                .collect();

            json_breadcrumbs = Some(serde_json::Value::Array(items));
        }
    }

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

    // Extract request context
    let (url, method, headers) = if let Some(request) = event.request.value() {
        (
            request.url.as_str().map(|s| s.to_string()),
            request.method.as_str().map(|s| s.to_string()),
            None, // Skip headers serialization - complex type
        )
    } else {
        (None, None, None)
    };

    Ok(CreateErrorEventData {
        source: Some("sentry".to_string()),
        raw_sentry_event: Some(raw_event),
        exceptions: exceptions_list,
        exception_type,
        exception_value,
        stack_trace,
        url,
        user_agent: None, // Could extract from request headers
        referrer: None,
        method,
        headers,
        user_id,
        user_email,
        user_username,
        user_ip_address: user_ip,
        user_segment: None,
        session_id: None,
        user_context: None, // Skip user serialization - complex type
        browser: None,
        browser_version: None,
        operating_system: None,
        operating_system_version: None,
        device_type: None,
        screen_width: None,
        screen_height: None,
        viewport_width: None,
        viewport_height: None,
        request_context: None, // Skip request serialization - complex type
        extra_context: None,   // Skip extra/tags serialization for now - complex types
        release_version: event.release.as_str().map(|r| r.to_string()),
        build_number: None,
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
        sdk_integrations: None, // Skip SDK integrations - complex type
        platform: event.platform.as_str().map(|p| p.to_string()),
        transaction_name: event.transaction.as_str().map(|t| t.to_string()),
        breadcrumbs: json_breadcrumbs,
        request_cookies: None,
        request_query_string: None,
        request_data: None,
        contexts: None, // Skip contexts serialization - complex type
        os_name: None,
        os_version: None,
        os_build: None,
        os_kernel_version: None,
        device_arch: None,
        device_processor_count: None,
        device_processor_frequency: None,
        device_memory_size: None,
        device_free_memory: None,
        device_boot_time: None,
        runtime_name: None,
        runtime_version: None,
        app_start_time: None,
        app_memory: None,
        locale: None,
        timezone: None,
        project_id,
        environment_id,
        deployment_id,
        visitor_id: None,
        ip_geolocation_id: None,
    })
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

                    // Filename is NativeImagePath, convert using Debug format as fallback
                    if let Some(filename) = frame.filename.value() {
                        let filename_str = format!("{:?}", filename);
                        obj.insert(
                            "filename".to_string(),
                            serde_json::Value::String(filename_str),
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

/// Convert Relay's Value type to serde_json::Value
pub fn relay_value_to_json(value: &relay_protocol::Value) -> Option<serde_json::Value> {
    match value {
        relay_protocol::Value::Bool(b) => Some(serde_json::Value::Bool(*b)),
        relay_protocol::Value::I64(i) => Some(serde_json::Value::Number((*i).into())),
        relay_protocol::Value::U64(u) => Some(serde_json::Value::Number((*u).into())),
        relay_protocol::Value::F64(f) => {
            serde_json::Number::from_f64(*f).map(serde_json::Value::Number)
        }
        relay_protocol::Value::String(s) => Some(serde_json::Value::String(s.clone())),
        relay_protocol::Value::Array(arr) => {
            let items: Vec<serde_json::Value> = arr
                .iter()
                .filter_map(|annotated| annotated.value().and_then(relay_value_to_json))
                .collect();
            Some(serde_json::Value::Array(items))
        }
        relay_protocol::Value::Object(obj) => {
            let map: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .filter_map(|(k, annotated)| {
                    annotated
                        .value()
                        .and_then(relay_value_to_json)
                        .map(|v| (k.clone(), v))
                })
                .collect();
            Some(serde_json::Value::Object(map))
        }
    }
}
