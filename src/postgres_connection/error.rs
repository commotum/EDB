//! Shared driver error classification and sanitized diagnostics; no database policy.
use crate::{ErrorCategory, SemanticError};

pub(crate) fn postgres_error(code: &'static str, error: postgres::Error) -> SemanticError {
    let database_error = error.as_db_error();
    let sqlstate = database_error.map(|error| error.code().code());
    // PostgreSQL can report restart/failover as a server SQLSTATE before the
    // socket disappears. Those are transport availability, not corrupt SQL.
    let transport = sqlstate.is_none()
        || sqlstate.is_some_and(|state| {
            state.starts_with("08") || matches!(state, "57P01" | "57P02" | "57P03")
        });
    let category = match sqlstate {
        Some("23505" | "40001" | "40P01") => ErrorCategory::Conflict,
        Some("42501") => ErrorCategory::Forbidden,
        Some("57014") => ErrorCategory::Interrupted,
        Some("55P03") => ErrorCategory::Busy,
        Some(state) if state.starts_with("08") || matches!(state, "57P01" | "57P02" | "57P03") => {
            ErrorCategory::Unavailable
        }
        Some(_) => ErrorCategory::Fault,
        None => ErrorCategory::Unavailable,
    };
    let mut semantic = if let Some(database_error) = database_error {
        let mut semantic = SemanticError::new(
            category,
            code,
            sanitize_postgres_message(database_error.message()),
        )
        .detail("postgres_sqlstate", database_error.code().code());
        for (name, value) in [
            ("postgres_constraint", database_error.constraint()),
            ("postgres_schema", database_error.schema()),
            ("postgres_table", database_error.table()),
            ("postgres_column", database_error.column()),
            ("postgres_routine", database_error.routine()),
        ] {
            if let Some(value) = value {
                semantic = semantic.detail(name, sanitize_postgres_identifier(value));
            }
        }
        semantic
    } else {
        // Driver/transport Display strings can contain connection locators.
        // The stable operation code and transport marker retain actionable
        // structure without echoing credentials or DSNs.
        SemanticError::new(category, code, "PostgreSQL transport error")
    };
    if transport {
        semantic = semantic.detail("postgres_transport", "true");
    }
    semantic
}

fn sanitize_postgres_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect()
}

fn sanitize_postgres_message(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(512));
    let mut quoted = None;
    for character in value.chars() {
        if output.len() >= 512 {
            break;
        }
        if let Some(delimiter) = quoted {
            if character == delimiter {
                quoted = None;
                output.push_str("<redacted>");
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quoted = Some(character);
        } else if character.is_control() {
            output.push(' ');
        } else {
            output.push(character);
        }
    }
    if quoted.is_some() {
        output.push_str("<redacted>");
    }
    let output = output.trim();
    if output.is_empty() {
        "PostgreSQL server error".to_owned()
    } else {
        output.to_owned()
    }
}

pub(crate) fn is_postgres_connection_error(error: &SemanticError) -> bool {
    error.category == ErrorCategory::Unavailable
        && error
            .details
            .get("postgres_transport")
            .is_some_and(|value| value == "true")
}
