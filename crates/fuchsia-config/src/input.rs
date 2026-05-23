//! Input value types for workflow node configuration.
//!
//! All input values are template strings that get resolved by minijinja at runtime.
//! The resolved string is then parsed into the expected type based on the component's
//! input schema.
//!
//! # Examples
//!
//! ```json
//! {
//!   "email": "{{ user.email }}",
//!   "count": "{{ items | length }}",
//!   "message": "Hello {{ name | title }}!",
//!   "static_value": "42"
//! }
//! ```
//!
//! After template resolution, values are parsed according to the schema:
//! - `"email"` (schema: string) → `"test@example.com"`
//! - `"count"` (schema: integer) → `42`
//! - `"message"` (schema: string) → `"Hello John!"`
//! - `"static_value"` (schema: integer) → `42`

/// An input value is a template string that gets resolved at runtime.
///
/// The template is processed by minijinja against upstream node data,
/// then the result is parsed into the expected type based on the
/// component's input schema.
pub type InputValue = String;
