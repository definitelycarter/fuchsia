//! Input resolution using minijinja templates.
//!
//! Resolves node inputs by rendering minijinja templates against upstream node data,
//! then coerces the resolved strings to typed values according to the component's
//! input schema.
//!
//! # Flow
//! 1. Template resolution: `HashMap<String, String>` -> render templates to strings
//! 2. Schema coercion: `serde_json::Value` -> parse strings to typed values
//!
//! # Single Upstream Node
//! When a node has one upstream, the context is the upstream node's `data` directly:
//! ```json
//! { "recipient": "{{ email }}", "greeting": "Hello {{ name | title }}!" }
//! ```
//!
//! # Join Node (Multiple Upstream)
//! When a node has multiple upstream nodes, context is keyed by node name:
//! ```json
//! { "user_email": "{{ fetch_user.email }}", "config": "{{ get_config.setting }}" }
//! ```

use std::collections::HashMap;

use minijinja::{Environment, Value};
use serde_json::json;

use crate::error::OrchestratorError;

/// JSON Schema type definitions for input coercion.
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaType {
    String,
    Number,
    Integer,
    Boolean,
    Null,
    Array,
    Object,
}

/// Extract schema types from a JSON Schema object.
///
/// Parses a JSON Schema and returns a map of property names to their types.
/// Only handles simple object schemas with "properties" - complex schemas
/// (anyOf, oneOf, etc.) are not yet supported.
pub fn extract_schema_types(json_schema: &serde_json::Value) -> HashMap<String, SchemaType> {
    let mut types = HashMap::new();

    if let Some(properties) = json_schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop_schema) in properties {
            if let Some(type_str) = prop_schema.get("type").and_then(|t| t.as_str()) {
                let schema_type = match type_str {
                    "string" => SchemaType::String,
                    "number" => SchemaType::Number,
                    "integer" => SchemaType::Integer,
                    "boolean" => SchemaType::Boolean,
                    "null" => SchemaType::Null,
                    "array" => SchemaType::Array,
                    "object" => SchemaType::Object,
                    _ => SchemaType::String, // Default to string for unknown types
                };
                types.insert(name.clone(), schema_type);
            }
        }
    }

    types
}

/// Resolve a node's inputs against upstream data.
///
/// Each input value is a template string that gets rendered against the upstream
/// context. The result is a map of input names to resolved strings.
pub fn resolve_inputs(
    node_id: &str,
    inputs: &HashMap<String, String>,
    upstream_data: &HashMap<String, serde_json::Value>,
    is_join: bool,
) -> Result<HashMap<String, String>, OrchestratorError> {
    let env = Environment::new();

    // Build the context based on whether this is a join node
    let context = if is_join {
        // Join node: context is keyed by upstream node names
        let ctx: serde_json::Map<String, serde_json::Value> = upstream_data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        serde_json::Value::Object(ctx)
    } else {
        // Single upstream: context is the upstream data directly
        upstream_data.values().next().cloned().unwrap_or(json!({}))
    };

    let ctx_value = Value::from_serialize(&context);

    let mut resolved = HashMap::new();

    for (key, template) in inputs {
        let value = resolve_template(&env, node_id, key, template, &ctx_value)?;
        resolved.insert(key.clone(), value);
    }

    Ok(resolved)
}

/// Resolve a template string.
fn resolve_template(
    env: &Environment,
    node_id: &str,
    input_key: &str,
    template: &str,
    context: &Value,
) -> Result<String, OrchestratorError> {
    env.render_str(template, context.clone())
        .map_err(|e| OrchestratorError::InputResolution {
            node_id: node_id.to_string(),
            message: format!("failed to resolve input '{}': {}", input_key, e),
        })
}

/// Coerce resolved string inputs to typed JSON values according to a schema.
pub fn coerce_inputs(
    node_id: &str,
    resolved: &HashMap<String, String>,
    schema: &HashMap<String, SchemaType>,
) -> Result<serde_json::Value, OrchestratorError> {
    let mut result = serde_json::Map::new();

    for (key, value) in resolved {
        let schema_type = schema.get(key).unwrap_or(&SchemaType::String);
        let typed_value = coerce_value(node_id, key, value, schema_type)?;
        result.insert(key.clone(), typed_value);
    }

    Ok(serde_json::Value::Object(result))
}

/// Coerce a single string value to a typed JSON value.
fn coerce_value(
    node_id: &str,
    input_key: &str,
    value: &str,
    schema_type: &SchemaType,
) -> Result<serde_json::Value, OrchestratorError> {
    match schema_type {
        SchemaType::String => Ok(serde_json::Value::String(value.to_string())),

        SchemaType::Number => value
            .parse::<f64>()
            .map(|n| {
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            })
            .map_err(|_| OrchestratorError::InputResolution {
                node_id: node_id.to_string(),
                message: format!("input '{}' expected number, got '{}'", input_key, value),
            }),

        SchemaType::Integer => value
            .parse::<i64>()
            .map(|n| serde_json::Value::Number(n.into()))
            .map_err(|_| OrchestratorError::InputResolution {
                node_id: node_id.to_string(),
                message: format!("input '{}' expected integer, got '{}'", input_key, value),
            }),

        SchemaType::Boolean => match value.to_lowercase().as_str() {
            "true" => Ok(serde_json::Value::Bool(true)),
            "false" => Ok(serde_json::Value::Bool(false)),
            _ => Err(OrchestratorError::InputResolution {
                node_id: node_id.to_string(),
                message: format!("input '{}' expected boolean, got '{}'", input_key, value),
            }),
        },

        SchemaType::Null => {
            if value.is_empty() || value == "null" {
                Ok(serde_json::Value::Null)
            } else {
                Err(OrchestratorError::InputResolution {
                    node_id: node_id.to_string(),
                    message: format!("input '{}' expected null, got '{}'", input_key, value),
                })
            }
        }

        SchemaType::Array => {
            serde_json::from_str(value).map_err(|e| OrchestratorError::InputResolution {
                node_id: node_id.to_string(),
                message: format!("input '{}' expected array: {}", input_key, e),
            })
        }

        SchemaType::Object => {
            serde_json::from_str(value).map_err(|e| OrchestratorError::InputResolution {
                node_id: node_id.to_string(),
                message: format!("input '{}' expected object: {}", input_key, e),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_single_upstream() {
        let mut inputs = HashMap::new();
        inputs.insert("recipient".to_string(), "{{ email }}".to_string());
        inputs.insert(
            "greeting".to_string(),
            "Hello {{ name | title }}!".to_string(),
        );

        let mut upstream_data = HashMap::new();
        upstream_data.insert(
            "fetch_user".to_string(),
            json!({
                "email": "test@example.com",
                "name": "john doe"
            }),
        );

        let result = resolve_inputs("send_email", &inputs, &upstream_data, false).unwrap();

        assert_eq!(result["recipient"], "test@example.com");
        assert_eq!(result["greeting"], "Hello John Doe!");
    }

    #[test]
    fn test_resolve_join_node() {
        let mut inputs = HashMap::new();
        inputs.insert(
            "user_email".to_string(),
            "{{ fetch_user.email }}".to_string(),
        );
        inputs.insert(
            "config_value".to_string(),
            "{{ get_config.setting }}".to_string(),
        );

        let mut upstream_data = HashMap::new();
        upstream_data.insert(
            "fetch_user".to_string(),
            json!({ "email": "test@example.com" }),
        );
        upstream_data.insert("get_config".to_string(), json!({ "setting": "enabled" }));

        let result = resolve_inputs("process", &inputs, &upstream_data, true).unwrap();

        assert_eq!(result["user_email"], "test@example.com");
        assert_eq!(result["config_value"], "enabled");
    }

    #[test]
    fn test_resolve_literal_values() {
        let mut inputs = HashMap::new();
        inputs.insert("count".to_string(), "42".to_string());
        inputs.insert("enabled".to_string(), "true".to_string());
        inputs.insert("message".to_string(), "hello world".to_string());

        let upstream_data = HashMap::new();

        let result = resolve_inputs("node", &inputs, &upstream_data, false).unwrap();

        assert_eq!(result["count"], "42");
        assert_eq!(result["enabled"], "true");
        assert_eq!(result["message"], "hello world");
    }

    #[test]
    fn test_resolve_template_with_number() {
        let mut inputs = HashMap::new();
        inputs.insert("from_template".to_string(), "{{ count }}".to_string());

        let mut upstream_data = HashMap::new();
        upstream_data.insert("prev".to_string(), json!({ "count": 100 }));

        let result = resolve_inputs("node", &inputs, &upstream_data, false).unwrap();

        assert_eq!(result["from_template"], "100");
    }

    #[test]
    fn test_empty_upstream() {
        let mut inputs = HashMap::new();
        inputs.insert("static".to_string(), "hello".to_string());

        let upstream_data = HashMap::new();

        let result = resolve_inputs("node", &inputs, &upstream_data, false).unwrap();

        assert_eq!(result["static"], "hello");
    }

    #[test]
    fn test_minijinja_filters() {
        let mut inputs = HashMap::new();
        inputs.insert("upper".to_string(), "{{ name | upper }}".to_string());
        inputs.insert("lower".to_string(), "{{ name | lower }}".to_string());

        let mut upstream_data = HashMap::new();
        upstream_data.insert("prev".to_string(), json!({ "name": "Test" }));

        let result = resolve_inputs("node", &inputs, &upstream_data, false).unwrap();

        assert_eq!(result["upper"], "TEST");
        assert_eq!(result["lower"], "test");
    }

    #[test]
    fn test_mixed_template_and_literal() {
        let mut inputs = HashMap::new();
        inputs.insert(
            "greeting".to_string(),
            "Hello {{ name }}, you have {{ count }} messages".to_string(),
        );

        let mut upstream_data = HashMap::new();
        upstream_data.insert("prev".to_string(), json!({ "name": "Alice", "count": 5 }));

        let result = resolve_inputs("node", &inputs, &upstream_data, false).unwrap();

        assert_eq!(result["greeting"], "Hello Alice, you have 5 messages");
    }

    #[test]
    fn test_coerce_string() {
        let mut resolved = HashMap::new();
        resolved.insert("name".to_string(), "hello world".to_string());

        let mut schema = HashMap::new();
        schema.insert("name".to_string(), SchemaType::String);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["name"], "hello world");
    }

    #[test]
    fn test_coerce_integer() {
        let mut resolved = HashMap::new();
        resolved.insert("count".to_string(), "42".to_string());

        let mut schema = HashMap::new();
        schema.insert("count".to_string(), SchemaType::Integer);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["count"], 42);
    }

    #[test]
    fn test_coerce_number() {
        let mut resolved = HashMap::new();
        resolved.insert("price".to_string(), "19.99".to_string());

        let mut schema = HashMap::new();
        schema.insert("price".to_string(), SchemaType::Number);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["price"], 19.99);
    }

    #[test]
    fn test_coerce_boolean() {
        let mut resolved = HashMap::new();
        resolved.insert("enabled".to_string(), "true".to_string());
        resolved.insert("disabled".to_string(), "false".to_string());
        resolved.insert("upper".to_string(), "TRUE".to_string());

        let mut schema = HashMap::new();
        schema.insert("enabled".to_string(), SchemaType::Boolean);
        schema.insert("disabled".to_string(), SchemaType::Boolean);
        schema.insert("upper".to_string(), SchemaType::Boolean);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["enabled"], true);
        assert_eq!(result["disabled"], false);
        assert_eq!(result["upper"], true);
    }

    #[test]
    fn test_coerce_null() {
        let mut resolved = HashMap::new();
        resolved.insert("empty".to_string(), "".to_string());
        resolved.insert("null_str".to_string(), "null".to_string());

        let mut schema = HashMap::new();
        schema.insert("empty".to_string(), SchemaType::Null);
        schema.insert("null_str".to_string(), SchemaType::Null);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["empty"], serde_json::Value::Null);
        assert_eq!(result["null_str"], serde_json::Value::Null);
    }

    #[test]
    fn test_coerce_array() {
        let mut resolved = HashMap::new();
        resolved.insert("items".to_string(), "[1, 2, 3]".to_string());

        let mut schema = HashMap::new();
        schema.insert("items".to_string(), SchemaType::Array);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["items"], json!([1, 2, 3]));
    }

    #[test]
    fn test_coerce_object() {
        let mut resolved = HashMap::new();
        resolved.insert("config".to_string(), r#"{"key": "value"}"#.to_string());

        let mut schema = HashMap::new();
        schema.insert("config".to_string(), SchemaType::Object);

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["config"], json!({"key": "value"}));
    }

    #[test]
    fn test_coerce_invalid_integer() {
        let mut resolved = HashMap::new();
        resolved.insert("count".to_string(), "not a number".to_string());

        let mut schema = HashMap::new();
        schema.insert("count".to_string(), SchemaType::Integer);

        let result = coerce_inputs("node", &resolved, &schema);
        assert!(result.is_err());
    }

    #[test]
    fn test_coerce_invalid_boolean() {
        let mut resolved = HashMap::new();
        resolved.insert("flag".to_string(), "yes".to_string());

        let mut schema = HashMap::new();
        schema.insert("flag".to_string(), SchemaType::Boolean);

        let result = coerce_inputs("node", &resolved, &schema);
        assert!(result.is_err());
    }

    #[test]
    fn test_coerce_defaults_to_string() {
        let mut resolved = HashMap::new();
        resolved.insert("unknown".to_string(), "some value".to_string());

        // Empty schema - should default to string
        let schema = HashMap::new();

        let result = coerce_inputs("node", &resolved, &schema).unwrap();
        assert_eq!(result["unknown"], "some value");
    }
}
