pub mod ast;
pub mod eval;
pub mod funcs;
pub mod parser;
pub mod path;

use ast::{Expr, ExprError};
use serde_json::Value;

/// Parse a single expression string into an AST.
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    parser::Parser::new(src)?.parse()
}

/// Parse and evaluate an expression against a JSON data context.
pub fn evaluate(src: &str, data: &Value) -> Result<Value, ExprError> {
    let expr = parse(src)?;
    eval::eval(&expr, data, 0)
}

/// Evaluate a pre-parsed expression against a JSON data context.
pub fn apply(expr: &Expr, data: &Value) -> Result<Value, ExprError> {
    eval::eval(expr, data, 0)
}

/// Evaluate an expression that is expected to yield a boolean (for guards/conditions).
pub fn truthy(src: &str, data: &Value) -> Result<bool, ExprError> {
    let v = evaluate(src, data)?;
    Ok(funcs::truthy(&v))
}

/// Resolve a JSON path like `$.a.b[1]` against a value, returning Null when missing.
pub fn get_path(data: &Value, path: &str) -> Value {
    match path::parse_path(path) {
        Ok(segs) => eval::resolve_path(data, &segs),
        Err(_) => Value::Null,
    }
}
