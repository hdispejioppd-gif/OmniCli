//! Convert JSON Schema (subset) into llama.cpp GBNF grammar.

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("unsupported schema construct at {path}: {detail}")]
    Unsupported { path: String, detail: String },
    #[error("empty schema")]
    Empty,
}

struct Ctx {
    counter: usize,
    rules: Vec<String>,
}

impl Ctx {
    fn new() -> Self {
        Self {
            counter: 0,
            rules: vec![],
        }
    }

    fn next_rule(&mut self, body: String) -> String {
        let name = format!("r{}", self.counter);
        self.counter += 1;
        self.rules.push(format!("{} ::= {}", name, body));
        name
    }
}

/// Convert a JSON Schema (subset) into llama.cpp GBNF grammar string.
pub fn json_schema_to_gbnf(schema: &Value) -> Result<String, SchemaError> {
    let mut ctx = Ctx::new();

    let root_name = schema_rule(schema, &mut ctx, "$")?;

    let mut out = String::new();
    // Built-in rules
    out.push_str("ws ::= [ \\t\\n]*\n");
    out.push_str("string ::= \"\\\"\" ([^\"\\\\] | \"\\\\\" [\"\\\\/bfnrt])* \"\\\"\"\n");
    out.push_str("integer ::= \"-\"? [0-9]+\n");
    out.push_str("number ::= \"-\"? [0-9]+ (\".\" [0-9]+)? ([eE] [+\\-]? [0-9]+)?\n");
    out.push_str("boolean ::= \"true\" | \"false\"\n");

    for rule in &ctx.rules {
        out.push_str(rule);
        out.push('\n');
    }

    out.push_str(&format!("root ::= {} ws\n", root_name));
    Ok(out)
}

fn schema_rule(schema: &Value, ctx: &mut Ctx, path: &str) -> Result<String, SchemaError> {
    let ty = schema.get("type").and_then(|v| v.as_str());
    match ty {
        Some("object") => object_rule(schema, ctx, path),
        Some("string") => Ok("string".to_string()),
        Some("integer") => Ok("integer".to_string()),
        Some("number") => Ok("number".to_string()),
        Some("boolean") => Ok("boolean".to_string()),
        Some("array") => array_rule(schema, ctx, path),
        _ => {
            if schema.get("enum").is_some() {
                enum_rule(schema, path)
            } else {
                Err(SchemaError::Unsupported {
                    path: path.to_string(),
                    detail: format!(
                        "type must be object/string/integer/number/boolean/array, got {:?}",
                        ty
                    ),
                })
            }
        }
    }
}

fn object_rule(schema: &Value, ctx: &mut Ctx, path: &str) -> Result<String, SchemaError> {
    let props = schema.get("properties").and_then(|v| v.as_object());
    let required: Vec<String> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let props = match props {
        Some(p) => p,
        None => {
            return Err(SchemaError::Unsupported {
                path: path.to_string(),
                detail: "object without properties".into(),
            });
        }
    };

    if !required.is_empty() && !required.iter().all(|r| props.contains_key(r)) {
        return Err(SchemaError::Unsupported {
            path: path.to_string(),
            detail: "required references missing property".into(),
        });
    }

    let mut parts = vec!["\"{\" ws".to_string()];
    let prop_names: Vec<&String> = props.keys().collect();

    for (i, name) in prop_names.iter().enumerate() {
        if i > 0 {
            parts.push("\",\" ws".to_string());
        }
        let prop_path = format!("{}.{}", path, name);
        let prop_schema = &props[*name];
        let rule_ref = schema_rule(prop_schema, ctx, &prop_path)?;
        let escaped = escape_gbnf(name);
        parts.push(format!(
            "\"\\\"{}\\\"\" ws \":\" ws {} ws",
            escaped, rule_ref
        ));
    }

    parts.push("\"}\"".to_string());
    Ok(ctx.next_rule(parts.join(" ")))
}

fn array_rule(schema: &Value, ctx: &mut Ctx, path: &str) -> Result<String, SchemaError> {
    let items = schema.get("items");
    let items = match items {
        Some(i) => i,
        None => {
            return Err(SchemaError::Unsupported {
                path: path.to_string(),
                detail: "array without items".into(),
            });
        }
    };

    let item_path = format!("{}[*]", path);
    let item_rule = schema_rule(items, ctx, &item_path)?;

    Ok(ctx.next_rule(format!(
        "\"[\" ws ({} (ws \",\" ws {})*)? ws \"]\"",
        item_rule, item_rule
    )))
}

fn enum_rule(schema: &Value, path: &str) -> Result<String, SchemaError> {
    let variants = schema.get("enum").and_then(|v| v.as_array());
    let variants = match variants {
        Some(v) => v,
        None => {
            return Err(SchemaError::Unsupported {
                path: path.to_string(),
                detail: "enum without variants".into(),
            });
        }
    };

    let strings: Vec<String> = variants
        .iter()
        .map(|v| {
            let s = v.as_str().unwrap_or("");
            format!("\"\\\"{}\\\"\"", escape_gbnf(s))
        })
        .collect();

    if strings.is_empty() {
        return Err(SchemaError::Empty);
    }

    Ok(strings.join(" | "))
}

fn escape_gbnf(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_object_produces_gbnf() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        });
        let g = json_schema_to_gbnf(&schema).unwrap();
        assert!(g.contains("name"));
        assert!(g.contains("integer"));
        assert!(g.contains("root ::="));
    }

    #[test]
    fn enum_produces_alternatives() {
        let schema = json!({ "enum": ["a", "b", "c"] });
        let g = json_schema_to_gbnf(&schema).unwrap();
        assert!(g.contains("a"));
        assert!(g.contains("|"));
    }

    #[test]
    fn unsupported_construct_errors() {
        let schema = json!({ "oneOf": [{ "type": "string" }] });
        assert!(json_schema_to_gbnf(&schema).is_err());
    }

    #[test]
    fn array_of_strings_produces_repetition() {
        let schema = json!({
            "type": "array",
            "items": { "type": "string" }
        });
        let g = json_schema_to_gbnf(&schema).unwrap();
        assert!(g.contains("string"));
        assert!(g.contains("ws \",\" ws"));
    }
}
