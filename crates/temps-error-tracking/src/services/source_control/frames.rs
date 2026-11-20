// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The application frames of a stored error event, innermost first, and the
//! repository path each one points at.

use serde_json::Value;

/// Most frames read from one event.
const MAX_FRAMES: usize = 200;

/// One frame of the event's main exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFrame {
    pub filename: String,
    pub function: Option<String>,
    pub lineno: Option<i64>,
    /// The SDK marked it as application code.
    pub in_app: bool,
}

fn frame(value: &Value) -> Option<AppFrame> {
    let filename = value
        .get("filename")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .or_else(|| value.get("abs_path").and_then(Value::as_str))?
        .to_string();
    Some(AppFrame {
        filename,
        function: value
            .get("function")
            .and_then(Value::as_str)
            .map(str::to_string),
        lineno: value.get("lineno").and_then(Value::as_i64),
        in_app: value.get("in_app").and_then(Value::as_bool) == Some(true),
    })
}

fn frames_of(stack: Option<&Value>) -> Option<&Vec<Value>> {
    let stack = stack?;
    stack
        .get("frames")
        .and_then(Value::as_array)
        .or_else(|| stack.as_array())
}

/// The frames of the event's main exception, innermost first. Only the
/// frames the SDK marked in-app when it marked any.
///
/// Sentry events keep the raw event under `sentry`, whose frames run
/// outermost first and whose last exception is the one raised; native
/// events keep `stack_trace`, innermost first.
pub fn app_frames(data: &Value) -> Vec<AppFrame> {
    app_frames_from(data.get("sentry"), data.get("stack_trace"))
}

/// [`app_frames`] over the raw Sentry event and the native stack of an
/// event being ingested.
pub fn app_frames_from(sentry: Option<&Value>, stack_trace: Option<&Value>) -> Vec<AppFrame> {
    let exceptions = sentry.and_then(|s| s.get("exception")).and_then(|e| {
        e.as_array()
            .or_else(|| e.get("values").and_then(Value::as_array))
    });
    let mut frames: Vec<AppFrame> = match exceptions
        .and_then(|values| values.last())
        .and_then(|exception| frames_of(exception.get("stacktrace")))
        .or_else(|| frames_of(sentry.and_then(|s| s.get("stacktrace"))))
    {
        Some(frames) => frames
            .iter()
            .rev()
            .take(MAX_FRAMES)
            .filter_map(frame)
            .collect(),
        None => frames_of(stack_trace)
            .map(|frames| frames.iter().take(MAX_FRAMES).filter_map(frame).collect())
            .unwrap_or_default(),
    };
    if frames.iter().any(|f| f.in_app) {
        frames.retain(|f| f.in_app);
    }
    frames
}

/// The path in the repository a frame's file is at, or `None` for a file
/// that is not the project's source (a URL, a dependency, a runtime file).
///
/// `source_root` is the project's `error_source_root`: the directory the
/// code runs from, stripped from absolute paths.
pub fn repository_path(filename: &str, source_root: Option<&str>) -> Option<String> {
    let mut path = filename.trim();
    if path.is_empty()
        || path.starts_with('<')
        || path.starts_with("http://")
        || path.starts_with("https://")
        || path.starts_with("node:")
        || path.starts_with("internal/")
    {
        return None;
    }
    for prefix in ["webpack-internal:///", "webpack:///", "app:///", "file://"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest;
        }
    }
    if let Some(root) = source_root.map(|r| r.trim().trim_end_matches('/')) {
        if !root.is_empty() {
            if let Some(rest) = path.strip_prefix(root) {
                path = rest;
            }
        }
    }
    let path = path.trim_start_matches("./").trim_start_matches('/');
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let dependency = [
        "node_modules/",
        "site-packages/",
        "dist-packages/",
        "vendor/",
        ".cargo/registry/",
        "gems/",
    ];
    if path.is_empty()
        || path.contains("..")
        || path.contains('\\')
        || dependency.iter().any(|d| path.contains(d))
    {
        return None;
    }
    Some(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sentry_frames_come_innermost_first_and_only_in_app_when_marked() {
        let data = json!({"sentry": {"exception": {"values": [
            {"type": "Cause", "stacktrace": {"frames": [{"filename": "cause.js"}]}},
            {"type": "TypeError", "stacktrace": {"frames": [
                {"filename": "node_modules/lib/index.js", "in_app": false},
                {"filename": "src/app.ts", "function": "main", "lineno": 3, "in_app": true},
                {"filename": "src/cart.ts", "function": "total", "lineno": 41, "in_app": true}
            ]}}
        ]}}});
        let frames = app_frames(&data);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].filename, "src/cart.ts");
        assert_eq!(frames[0].lineno, Some(41));
        assert_eq!(frames[1].function.as_deref(), Some("main"));
    }

    #[test]
    fn native_frames_are_already_innermost_first() {
        let data = json!({"stack_trace": [
            {"filename": "app/models/cart.rb", "lineno": 10},
            {"abs_path": "/srv/app/app/controllers/carts_controller.rb"}
        ]});
        let frames = app_frames(&data);
        assert_eq!(frames[0].filename, "app/models/cart.rb");
        assert_eq!(
            frames[1].filename,
            "/srv/app/app/controllers/carts_controller.rb"
        );
        assert!(app_frames(&json!({})).is_empty());
    }

    #[test]
    fn repository_paths_drop_bundler_prefixes_the_source_root_and_dependencies() {
        assert_eq!(
            repository_path("webpack:///./src/cart.ts", None).as_deref(),
            Some("src/cart.ts")
        );
        assert_eq!(
            repository_path("/srv/app/lib/cart.py", Some("/srv/app/")).as_deref(),
            Some("lib/cart.py")
        );
        assert_eq!(
            repository_path("app:///src/cart.ts?v=3", None).as_deref(),
            Some("src/cart.ts")
        );
        for outside in [
            "https://shop.example/static/main.js",
            "<anonymous>",
            "node:internal/process",
            "/usr/lib/python3/site-packages/requests/api.py",
            "../secrets.txt",
            "",
        ] {
            assert_eq!(repository_path(outside, None), None, "{outside}");
        }
    }
}
