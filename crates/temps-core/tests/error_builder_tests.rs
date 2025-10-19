use temps_core::error_builder::{
    ErrorBuilder, bad_request, conflict, forbidden, internal_server_error, not_found, unauthorized,
};
use axum::http::StatusCode;

#[test]
fn test_error_builder_basic() {
    let error = ErrorBuilder::new(StatusCode::BAD_REQUEST)
        .type_("https://example.com/probs/validation-error")
        .title("Validation Error")
        .detail("The request contains invalid data")
        .instance("/users/123")
        .build();

    assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://example.com/probs/validation-error");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Validation Error");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "The request contains invalid data");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/users/123");
}

#[test]
fn test_error_builder_with_values() {
    let error = ErrorBuilder::new(StatusCode::UNPROCESSABLE_ENTITY)
        .title("Validation Failed")
        .value("field", "email")
        .value("reason", "invalid format")
        .value("code", 422)
        .build();

    assert_eq!(error.status_code, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Validation Failed");

    // Check that body contains our values
    assert!(error.body.contains_key("field"));
    assert!(error.body.contains_key("reason"));
    assert!(error.body.contains_key("code"));
    assert!(error.body.contains_key("timestamp"));
}

#[test]
fn test_internal_server_error_builder() {
    let error = internal_server_error().build();

    assert_eq!(error.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://temps.sh/probs/internal-server-error");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Internal Server Error");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "An unexpected error occurred while processing your request");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/error/internal-server-error");

    // Should contain error_code
    assert!(error.body.contains_key("error_code"));
    assert_eq!(error.body.get("error_code").unwrap().as_str().unwrap(), "INTERNAL_SERVER_ERROR");
}

#[test]
fn test_not_found_builder() {
    let error = not_found()
        .detail("User with ID 123 was not found")
        .build();

    assert_eq!(error.status_code, StatusCode::NOT_FOUND);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://temps.sh/probs/not-found");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Resource Not Found");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "User with ID 123 was not found");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/error/not-found");

    assert!(error.body.contains_key("error_code"));
    assert_eq!(error.body.get("error_code").unwrap().as_str().unwrap(), "NOT_FOUND");
}

#[test]
fn test_unauthorized_builder() {
    let error = unauthorized().build();

    assert_eq!(error.status_code, StatusCode::UNAUTHORIZED);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://temps.sh/probs/unauthorized");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Unauthorized");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "Authentication is required to access this resource");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/error/unauthorized");

    assert!(error.body.contains_key("error_code"));
    assert_eq!(error.body.get("error_code").unwrap().as_str().unwrap(), "UNAUTHORIZED");
}

#[test]
fn test_bad_request_builder() {
    let error = bad_request()
        .detail("Missing required field: email")
        .build();

    assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://temps.sh/probs/bad-request");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Bad Request");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "Missing required field: email");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/error/bad-request");
}

#[test]
fn test_forbidden_builder() {
    let error = forbidden().build();

    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://temps.sh/probs/forbidden");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Forbidden");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "You do not have permission to access this resource");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/error/forbidden");

    assert!(error.body.contains_key("error_code"));
    assert_eq!(error.body.get("error_code").unwrap().as_str().unwrap(), "FORBIDDEN");
}

#[test]
fn test_conflict_builder() {
    let error = conflict()
        .detail("User with this email already exists")
        .build();

    assert_eq!(error.status_code, StatusCode::CONFLICT);
    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "https://temps.sh/probs/conflict");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Conflict");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "User with this email already exists");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/error/conflict");

    assert!(error.body.contains_key("error_code"));
    assert_eq!(error.body.get("error_code").unwrap().as_str().unwrap(), "CONFLICT");
}

#[test]
fn test_error_builder_chaining() {
    let error = ErrorBuilder::new(StatusCode::BAD_REQUEST)
        .type_("custom-type")
        .title("Custom Title")
        .detail("Custom Detail")
        .instance("/custom/path")
        .value("custom_field", "custom_value")
        .build();

    assert_eq!(error.body.get("type").unwrap().as_str().unwrap(), "custom-type");
    assert_eq!(error.body.get("title").unwrap().as_str().unwrap(), "Custom Title");
    assert_eq!(error.body.get("detail").unwrap().as_str().unwrap(), "Custom Detail");
    assert_eq!(error.body.get("instance").unwrap().as_str().unwrap(), "/custom/path");
    assert_eq!(error.body.get("custom_field").unwrap().as_str().unwrap(), "custom_value");
    assert!(error.body.contains_key("timestamp"));
}