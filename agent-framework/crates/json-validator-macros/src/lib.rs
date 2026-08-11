use std::{env, fs, path::PathBuf};

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use serde_json::{Map, Value};
use syn::{parse::Parser as _, punctuated::Punctuated, LitStr, Token};

const SUPPORTED_KEYS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "items",
    "minimum",
    "trimmedNonEmpty",
    "description",
];

#[proc_macro]
/// Compile a manifest-relative JSON Schema file into a static validator.
///
/// Multiple string literals are concatenated, which lets wrapper macros build
/// conventional resource paths without runtime path handling.
pub fn validator(input: TokenStream) -> TokenStream {
    let parser = Punctuated::<LitStr, Token![,]>::parse_terminated;
    let paths = match parser.parse(input) {
        Ok(paths) if !paths.is_empty() => paths,
        Ok(_) => {
            return syn::Error::new(proc_macro2::Span::call_site(), "expected a schema path")
                .to_compile_error()
                .into()
        }
        Err(error) => return error.to_compile_error().into(),
    };
    let span = paths
        .first()
        .map(LitStr::span)
        .unwrap_or_else(proc_macro2::Span::call_site);
    let relative_path = paths.iter().map(LitStr::value).collect::<String>();
    match expand_validator(&relative_path) {
        Ok(tokens) => tokens.into(),
        Err(message) => syn::Error::new(span, message).to_compile_error().into(),
    }
}

fn expand_validator(relative_path: &str) -> Result<TokenStream2, String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is unavailable".to_owned())?;
    let path = PathBuf::from(manifest_dir).join(relative_path);
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let document: Value = serde_json::from_str(&source)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?;
    let schema = document
        .pointer("/function/parameters")
        .unwrap_or(&document);
    let schema = compile_schema(schema, "$schema")?;
    let tracked_path = path
        .to_str()
        .ok_or_else(|| format!("schema path is not valid UTF-8: {}", path.display()))?;
    Ok(quote!({
        const _: &str = ::core::include_str!(#tracked_path);
        ::json_validator::Validator::from_schema(#schema)
    }))
}

fn compile_schema(schema: &Value, path: &str) -> Result<TokenStream2, String> {
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path} must be a JSON object"))?;
    reject_unsupported_keys(object, path)?;

    let value_type = match object.get("type") {
        Some(Value::String(value_type)) => {
            let token = match value_type.as_str() {
                "object" => quote!(::json_validator::Type::Object),
                "array" => quote!(::json_validator::Type::Array),
                "string" => quote!(::json_validator::Type::String),
                "integer" => quote!(::json_validator::Type::Integer),
                "number" => quote!(::json_validator::Type::Number),
                "boolean" => quote!(::json_validator::Type::Boolean),
                "null" => quote!(::json_validator::Type::Null),
                other => return Err(format!("{path}.type '{other}' is unsupported")),
            };
            quote!(::core::option::Option::Some(#token))
        }
        Some(_) => return Err(format!("{path}.type must be a string")),
        None => quote!(::core::option::Option::None),
    };

    let required = string_array(object.get("required"), &format!("{path}.required"))?;
    let required = required.iter().map(|value| quote!(#value));

    let mut properties = Vec::new();
    if let Some(value) = object.get("properties") {
        let fields = value
            .as_object()
            .ok_or_else(|| format!("{path}.properties must be an object"))?;
        for (name, field_schema) in fields {
            let field_path = format!("{path}.properties.{name}");
            let compiled = compile_schema(field_schema, &field_path)?;
            properties.push(quote!(::json_validator::Property::new(#name, #compiled)));
        }
    }

    let additional_properties = match object.get("additionalProperties") {
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(format!("{path}.additionalProperties must be boolean")),
        None => true,
    };

    let allowed_values = string_array(object.get("enum"), &format!("{path}.enum"))?;
    if object.contains_key("enum") && allowed_values.is_empty() {
        return Err(format!("{path}.enum must not be empty"));
    }
    let allowed_values = allowed_values
        .iter()
        .map(|value| quote!(::json_validator::Literal::String(#value)));

    let items = match object.get("items") {
        Some(items) => {
            let compiled = compile_schema(items, &format!("{path}.items"))?;
            quote!(::core::option::Option::Some(&#compiled))
        }
        None => quote!(::core::option::Option::None),
    };

    let minimum = match object.get("minimum") {
        Some(value) => {
            let minimum = value
                .as_f64()
                .ok_or_else(|| format!("{path}.minimum must be a number"))?;
            quote!(::core::option::Option::Some(#minimum))
        }
        None => quote!(::core::option::Option::None),
    };

    let trimmed_non_empty = match object.get("trimmedNonEmpty") {
        Some(Value::Bool(value)) => {
            if object.get("type").and_then(Value::as_str) != Some("string") {
                return Err(format!(
                    "{path}.trimmedNonEmpty requires type to be 'string'"
                ));
            }
            *value
        }
        Some(_) => return Err(format!("{path}.trimmedNonEmpty must be boolean")),
        None => false,
    };

    Ok(quote!(::json_validator::Schema::new(
        #value_type,
        &[#(#required),*],
        &[#(#properties),*],
        #additional_properties,
        &[#(#allowed_values),*],
        #items,
        ::json_validator::Constraints::new(#minimum, #trimmed_non_empty),
    )))
}

fn reject_unsupported_keys(object: &Map<String, Value>, path: &str) -> Result<(), String> {
    for key in object.keys() {
        if !SUPPORTED_KEYS.contains(&key.as_str()) {
            return Err(format!("{path}.{key} is not supported by json-validator"));
        }
    }
    Ok(())
}

fn string_array(value: Option<&Value>, path: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{path} entries must be strings"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::compile_schema;

    #[test]
    fn unsupported_keywords_are_compile_errors() {
        let error = compile_schema(&json!({"type": "string", "maxLength": 4}), "$schema")
            .expect_err("unsupported keyword must fail");
        assert!(error.contains("maxLength"));
    }

    #[test]
    fn malformed_keyword_values_are_compile_errors() {
        let error = compile_schema(&json!({"type": "object", "required": "name"}), "$schema")
            .expect_err("malformed required must fail");
        assert!(error.contains("required must be an array"));

        let error = compile_schema(
            &json!({"type": "integer", "trimmedNonEmpty": true}),
            "$schema",
        )
        .expect_err("trimmedNonEmpty on an integer must fail");
        assert!(error.contains("requires type to be 'string'"));
    }
}
