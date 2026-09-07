use std::{collections::BTreeMap, env, fs, path::PathBuf};

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use serde_json::{Map, Value};
use syn::{
    parse::{Parse, ParseStream, Parser as _},
    punctuated::Punctuated,
    LitStr, Path, Token,
};

const SUPPORTED_KEYS: &[&str] = &[
    "$schema",
    "$defs",
    "$ref",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "const",
    "oneOf",
    "items",
    "minItems",
    "maxItems",
    "maxProperties",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "contentEncoding",
    "trimmedNonEmpty",
    "title",
    "description",
    "default",
];

#[proc_macro]
/// Compile a manifest-relative JSON Schema file into a static validator.
///
/// Multiple string literals are concatenated, which lets wrapper macros build
/// conventional resource paths without runtime path handling.
pub fn validator(input: TokenStream) -> TokenStream {
    expand_file_validator(input, quote!(::json_validator))
}

#[proc_macro]
#[doc(hidden)]
pub fn validator_with_runtime(input: TokenStream) -> TokenStream {
    let input = match syn::parse::<RuntimeFileInput>(input) {
        Ok(input) => input,
        Err(error) => return error.to_compile_error().into(),
    };
    let RuntimeFileInput { runtime, paths } = input;
    expand_file_paths(paths, quote!(#runtime))
}

#[proc_macro]
/// Compiles an inline JSON Schema string into a static validator.
pub fn validator_source(input: TokenStream) -> TokenStream {
    expand_source_validator(input, quote!(::json_validator))
}

#[proc_macro]
#[doc(hidden)]
pub fn validator_source_with_runtime(input: TokenStream) -> TokenStream {
    let input = match syn::parse::<RuntimeSourceInput>(input) {
        Ok(input) => input,
        Err(error) => return error.to_compile_error().into(),
    };
    let RuntimeSourceInput { runtime, source } = input;
    expand_source(source, quote!(#runtime))
}

fn expand_file_validator(input: TokenStream, runtime: TokenStream2) -> TokenStream {
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
    expand_file_paths(paths, runtime)
}

fn expand_file_paths(paths: Punctuated<LitStr, Token![,]>, runtime: TokenStream2) -> TokenStream {
    let span = paths
        .first()
        .map(LitStr::span)
        .unwrap_or_else(proc_macro2::Span::call_site);
    let relative_path = paths.iter().map(LitStr::value).collect::<String>();
    match expand_validator(&relative_path, &runtime) {
        Ok(tokens) => tokens.into(),
        Err(message) => syn::Error::new(span, message).to_compile_error().into(),
    }
}

fn expand_source_validator(input: TokenStream, runtime: TokenStream2) -> TokenStream {
    let source = match syn::parse::<LitStr>(input) {
        Ok(source) => source,
        Err(error) => return error.to_compile_error().into(),
    };
    expand_source(source, runtime)
}

fn expand_source(source: LitStr, runtime: TokenStream2) -> TokenStream {
    let document: Value = match serde_json::from_str(&source.value()) {
        Ok(document) => document,
        Err(error) => {
            return syn::Error::new(source.span(), format!("invalid JSON Schema: {error}"))
                .to_compile_error()
                .into()
        }
    };
    match compile_validator(&document, &document, &runtime) {
        Ok(validator) => validator.into(),
        Err(message) => syn::Error::new(source.span(), message)
            .to_compile_error()
            .into(),
    }
}

struct RuntimeFileInput {
    runtime: Path,
    paths: Punctuated<LitStr, Token![,]>,
}

impl Parse for RuntimeFileInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let runtime = input.parse()?;
        input.parse::<Token![;]>()?;
        let paths = Punctuated::<LitStr, Token![,]>::parse_terminated(input)?;
        if paths.is_empty() {
            return Err(input.error("expected a schema path"));
        }
        Ok(Self { runtime, paths })
    }
}

struct RuntimeSourceInput {
    runtime: Path,
    source: LitStr,
}

impl Parse for RuntimeSourceInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let runtime = input.parse()?;
        input.parse::<Token![;]>()?;
        let source = input.parse()?;
        Ok(Self { runtime, source })
    }
}

fn expand_validator(relative_path: &str, runtime: &TokenStream2) -> Result<TokenStream2, String> {
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
    let validator = compile_validator(schema, &document, runtime)?;
    let tracked_path = path
        .to_str()
        .ok_or_else(|| format!("schema path is not valid UTF-8: {}", path.display()))?;
    Ok(quote!({
        const _: &str = ::core::include_str!(#tracked_path);
        #validator
    }))
}

fn compile_validator(
    schema: &Value,
    document: &Value,
    runtime: &TokenStream2,
) -> Result<TokenStream2, String> {
    let definitions = document
        .get("$defs")
        .and_then(Value::as_object)
        .or_else(|| schema.get("$defs").and_then(Value::as_object));
    let definition_indexes: BTreeMap<&str, usize> = definitions
        .into_iter()
        .flat_map(|definitions| definitions.keys().enumerate())
        .map(|(index, name)| (name.as_str(), index))
        .collect();
    let compiled_definitions = definitions
        .into_iter()
        .flat_map(|definitions| definitions.iter())
        .map(|(name, definition)| {
            compile_schema(
                definition,
                &format!("$schema.$defs.{name}"),
                runtime,
                &definition_indexes,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let compiled_schema = compile_schema(schema, "$schema", runtime, &definition_indexes)?;
    Ok(quote!(#runtime::Validator::from_parts(
        &#compiled_schema,
        &[#(#compiled_definitions),*],
    )))
}

fn compile_schema(
    schema: &Value,
    path: &str,
    runtime: &TokenStream2,
    definition_indexes: &BTreeMap<&str, usize>,
) -> Result<TokenStream2, String> {
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path} must be a JSON object"))?;
    reject_unsupported_keys(object, path)?;

    if let Some(reference) = object.get("$ref") {
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "$ref" | "$schema" | "$defs" | "title" | "description" | "default"
            )
        }) {
            return Err(format!("{path}.$ref cannot have sibling keywords"));
        }
        let reference = reference
            .as_str()
            .ok_or_else(|| format!("{path}.$ref must be a string"))?;
        let name = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| format!("{path}.$ref must reference #/$defs/<name>"))?;
        let index = definition_indexes
            .get(name)
            .copied()
            .ok_or_else(|| format!("{path}.$ref does not resolve: {reference}"))?;
        return Ok(quote!(#runtime::Schema::reference(#index)));
    }

    let value_types = match object.get("type") {
        Some(Value::String(value_type)) => {
            vec![compile_type(value_type, path, runtime)?]
        }
        Some(Value::Array(types)) if !types.is_empty() => types
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| format!("{path}.type entries must be strings"))
                    .and_then(|value| compile_type(value, path, runtime))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{path}.type must be a string or non-empty array")),
        None => Vec::new(),
    };

    let required = string_array(object.get("required"), &format!("{path}.required"))?;
    if required.len() > 128 {
        return Err(format!("{path}.required supports at most 128 entries"));
    }
    let required = required.iter().map(|value| quote!(#value));

    let mut properties = Vec::new();
    if let Some(value) = object.get("properties") {
        let fields = value
            .as_object()
            .ok_or_else(|| format!("{path}.properties must be an object"))?;
        for (name, field_schema) in fields {
            let field_path = format!("{path}.properties.{name}");
            let compiled = compile_schema(field_schema, &field_path, runtime, definition_indexes)?;
            properties.push(quote!(#runtime::Property::new(#name, #compiled)));
        }
    }

    let additional_properties = match object.get("additionalProperties") {
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(format!("{path}.additionalProperties must be boolean")),
        None => true,
    };

    let allowed_values = literal_array(object.get("enum"), &format!("{path}.enum"), runtime)?;
    let constant = object
        .get("const")
        .map(|value| compile_literal(value, &format!("{path}.const"), runtime))
        .transpose()?;
    let constant = match constant {
        Some(value) => quote!(::core::option::Option::Some(#value)),
        None => quote!(::core::option::Option::None),
    };

    let alternatives = match object.get("oneOf") {
        Some(Value::Array(alternatives)) if !alternatives.is_empty() => alternatives
            .iter()
            .enumerate()
            .map(|(index, alternative)| {
                compile_schema(
                    alternative,
                    &format!("{path}.oneOf[{index}]"),
                    runtime,
                    definition_indexes,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{path}.oneOf must be a non-empty array")),
        None => Vec::new(),
    };

    let items = match object.get("items") {
        Some(items) => {
            let compiled =
                compile_schema(items, &format!("{path}.items"), runtime, definition_indexes)?;
            quote!(::core::option::Option::Some(&#compiled))
        }
        None => quote!(::core::option::Option::None),
    };

    let minimum = optional_number(object.get("minimum"), &format!("{path}.minimum"))?;
    let maximum = optional_number(object.get("maximum"), &format!("{path}.maximum"))?;
    let min_length = optional_usize(object.get("minLength"), &format!("{path}.minLength"))?;
    let max_length = optional_usize(object.get("maxLength"), &format!("{path}.maxLength"))?;
    let min_items = optional_usize(object.get("minItems"), &format!("{path}.minItems"))?;
    let max_items = optional_usize(object.get("maxItems"), &format!("{path}.maxItems"))?;
    let max_properties = optional_usize(
        object.get("maxProperties"),
        &format!("{path}.maxProperties"),
    )?;

    let pattern = match object.get("pattern") {
        Some(Value::String(pattern)) => compile_pattern(pattern, path, runtime)?,
        Some(_) => return Err(format!("{path}.pattern must be a string")),
        None => quote!(::core::option::Option::None),
    };
    let format = match object.get("format") {
        Some(Value::String(format)) if format == "date-time" => {
            quote!(::core::option::Option::Some(#runtime::StringFormat::DateTime))
        }
        Some(Value::String(format)) => {
            return Err(format!("{path}.format '{format}' is unsupported"))
        }
        Some(_) => return Err(format!("{path}.format must be a string")),
        None => quote!(::core::option::Option::None),
    };
    let content_encoding = match object.get("contentEncoding") {
        Some(Value::String(encoding)) if encoding == "base64" => {
            quote!(::core::option::Option::Some(#runtime::ContentEncoding::Base64))
        }
        Some(Value::String(encoding)) => {
            return Err(format!(
                "{path}.contentEncoding '{encoding}' is unsupported"
            ))
        }
        Some(_) => return Err(format!("{path}.contentEncoding must be a string")),
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

    Ok(quote!(#runtime::Schema::new(
        &[#(#value_types),*],
        &[#(#required),*],
        &[#(#properties),*],
        #additional_properties,
        &[#(#allowed_values),*],
        #constant,
        &[#(#alternatives),*],
        #items,
        #runtime::Constraints::new(
            #minimum,
            #maximum,
            #min_length,
            #max_length,
            #min_items,
            #max_items,
            #max_properties,
            #pattern,
            #format,
            #content_encoding,
            #trimmed_non_empty,
        ),
    )))
}

fn compile_type(value: &str, path: &str, runtime: &TokenStream2) -> Result<TokenStream2, String> {
    match value {
        "object" => Ok(quote!(#runtime::Type::Object)),
        "array" => Ok(quote!(#runtime::Type::Array)),
        "string" => Ok(quote!(#runtime::Type::String)),
        "integer" => Ok(quote!(#runtime::Type::Integer)),
        "number" => Ok(quote!(#runtime::Type::Number)),
        "boolean" => Ok(quote!(#runtime::Type::Boolean)),
        "null" => Ok(quote!(#runtime::Type::Null)),
        other => Err(format!("{path}.type '{other}' is unsupported")),
    }
}

fn compile_literal(
    value: &Value,
    path: &str,
    runtime: &TokenStream2,
) -> Result<TokenStream2, String> {
    match value {
        Value::String(value) => Ok(quote!(#runtime::Literal::String(#value))),
        Value::Bool(value) => Ok(quote!(#runtime::Literal::Boolean(#value))),
        Value::Null => Ok(quote!(#runtime::Literal::Null)),
        Value::Number(value) => value
            .as_f64()
            .map(|value| quote!(#runtime::Literal::Number(#value)))
            .ok_or_else(|| format!("{path} must be a finite JSON number")),
        _ => Err(format!("{path} only supports primitive JSON values")),
    }
}

fn literal_array(
    value: Option<&Value>,
    path: &str,
    runtime: &TokenStream2,
) -> Result<Vec<TokenStream2>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if values.is_empty() {
        return Err(format!("{path} must not be empty"));
    }
    values
        .iter()
        .map(|value| compile_literal(value, path, runtime))
        .collect()
}

fn optional_number(value: Option<&Value>, path: &str) -> Result<TokenStream2, String> {
    match value {
        Some(value) => value
            .as_f64()
            .map(|value| quote!(::core::option::Option::Some(#value)))
            .ok_or_else(|| format!("{path} must be a number")),
        None => Ok(quote!(::core::option::Option::None)),
    }
}

fn optional_usize(value: Option<&Value>, path: &str) -> Result<TokenStream2, String> {
    match value {
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .map(|value| quote!(::core::option::Option::Some(#value)))
            .ok_or_else(|| format!("{path} must be a non-negative usize")),
        None => Ok(quote!(::core::option::Option::None)),
    }
}

fn compile_pattern(
    pattern: &str,
    path: &str,
    runtime: &TokenStream2,
) -> Result<TokenStream2, String> {
    let pattern = match pattern {
        "^[A-Za-z0-9_.-]+$" => quote!(#runtime::StringPattern::AsciiIdentifier),
        "^https?://" => quote!(#runtime::StringPattern::HttpUrl),
        "^session-[0-9]+$" => quote!(#runtime::StringPattern::SessionId),
        "^run-[0-9]+$" => quote!(#runtime::StringPattern::RunId),
        "^input-[0-9]+$" => quote!(#runtime::StringPattern::InputId),
        "^command-[0-9]+$" => quote!(#runtime::StringPattern::CommandId),
        "^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]\\.[0-9]{3}Z$" => {
            quote!(#runtime::StringPattern::UtcMilliseconds)
        }
        _ => return Err(format!("{path}.pattern '{pattern}' is unsupported")),
    };
    Ok(quote!(::core::option::Option::Some(#pattern)))
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
    use std::collections::BTreeMap;

    use quote::quote;
    use serde_json::json;

    use super::compile_schema;

    #[test]
    fn unsupported_keywords_are_compile_errors() {
        let schema = json!({"type": "string", "exclusiveMaximum": 4});
        let error = compile_schema(
            &schema,
            "$schema",
            &quote!(::json_validator),
            &BTreeMap::new(),
        )
        .expect_err("unsupported keyword must fail");
        assert!(error.contains("exclusiveMaximum"));
    }

    #[test]
    fn malformed_keyword_values_are_compile_errors() {
        let schema = json!({"type": "object", "required": "name"});
        let error = compile_schema(
            &schema,
            "$schema",
            &quote!(::json_validator),
            &BTreeMap::new(),
        )
        .expect_err("malformed required must fail");
        assert!(error.contains("required must be an array"));

        let schema = json!({"type": "integer", "trimmedNonEmpty": true});
        let error = compile_schema(
            &schema,
            "$schema",
            &quote!(::json_validator),
            &BTreeMap::new(),
        )
        .expect_err("trimmedNonEmpty on an integer must fail");
        assert!(error.contains("requires type to be 'string'"));
    }
}
