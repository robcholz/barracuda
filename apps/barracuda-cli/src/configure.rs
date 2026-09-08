//! Interactive developer configuration for known Barracuda Plugins.

use std::{net::IpAddr, time::Duration};

use anyhow::{anyhow, bail, Context as _, Result};
use bytes::Bytes;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{
    body::Incoming,
    client::conn::http1,
    header::{CONTENT_LENGTH, CONTENT_TYPE, HOST},
    Request, StatusCode, Uri,
};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value};
use tokio::{net::TcpStream, time::timeout};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
struct Configuration {
    id: &'static str,
    title: &'static str,
    endpoint: &'static str,
    description: &'static str,
    fields: &'static [Field],
    batch: bool,
}

#[derive(Clone, Copy)]
struct Field {
    name: &'static str,
    label: &'static str,
    kind: FieldKind,
    default: Option<DefaultValue>,
    options: &'static [&'static str],
    optional: bool,
}

impl Field {
    const fn new(name: &'static str, label: &'static str, kind: FieldKind) -> Self {
        Self {
            name,
            label,
            kind,
            default: None,
            options: &[],
            optional: false,
        }
    }

    const fn with_default(mut self, value: DefaultValue) -> Self {
        self.default = Some(value);
        self
    }

    const fn with_options(mut self, options: &'static [&'static str]) -> Self {
        self.options = options;
        self
    }

    const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Text,
    Password,
    Url,
    Number,
    Boolean,
    Choice,
}

#[derive(Clone, Copy)]
enum DefaultValue {
    Text(&'static str),
    Number(u32),
    Boolean(bool),
}

const CONFIGURATIONS: &[Configuration] = &[
    Configuration {
        id: "agent",
        title: "模型配置",
        endpoint: "/api/model-api",
        description: "为指定 Agent 用途注册模型。每次提交一个模型配置。",
        fields: &[
            Field::new("backend", "接口协议", FieldKind::Choice)
                .with_options(&["openai_compatible", "anthropic_compatible"]),
            Field::new("purpose", "用途", FieldKind::Choice).with_options(&[
                "root_agent",
                "sub_agent",
                "memory",
                "compaction",
            ]),
            Field::new("default", "设为该用途默认模型", FieldKind::Boolean)
                .with_default(DefaultValue::Boolean(true)),
            Field::new("base_url", "API Base URL", FieldKind::Url),
            Field::new("model", "模型名称", FieldKind::Text),
            Field::new("api_key", "API Key", FieldKind::Password),
            Field::new("timeout_ms", "请求超时（毫秒）", FieldKind::Number)
                .with_default(DefaultValue::Number(120_000)),
            Field::new("max_tokens", "最大输出 tokens", FieldKind::Number)
                .with_default(DefaultValue::Number(8_192)),
            Field::new("image_max_bytes", "图片大小上限（字节）", FieldKind::Number)
                .with_default(DefaultValue::Number(524_288)),
        ],
        batch: true,
    },
    Configuration {
        id: "agent-websearch",
        title: "网页搜索",
        endpoint: "/api/tavily",
        description: "配置 Tavily 搜索服务；配置保存到设备存储。",
        fields: &[
            Field::new("api_key", "Tavily API Key", FieldKind::Password),
            Field::new("api_base", "API Base URL", FieldKind::Url)
                .with_default(DefaultValue::Text("https://api.tavily.com")),
        ],
        batch: false,
    },
    Configuration {
        id: "imessage-qq",
        title: "QQ",
        endpoint: "/api/gateway/qq",
        description: "配置 QQ Bot 消息通道；替换当前运行时通道。",
        fields: &[
            Field::new("app_id", "App ID", FieldKind::Text),
            Field::new("access_token", "Access Token", FieldKind::Password),
            Field::new("api_base", "API Base URL", FieldKind::Url)
                .with_default(DefaultValue::Text("https://api.sgroup.qq.com")),
        ],
        batch: false,
    },
    Configuration {
        id: "imessage-wechat",
        title: "微信",
        endpoint: "/api/gateway/wechat",
        description: "配置微信消息通道；替换当前运行时通道。",
        fields: &[
            Field::new("token", "Token", FieldKind::Password),
            Field::new("api_base", "API Base URL", FieldKind::Url)
                .with_default(DefaultValue::Text("https://ilinkai.weixin.qq.com")),
            Field::new("app_id", "App ID", FieldKind::Text).with_default(DefaultValue::Text("bot")),
            Field::new("client_version", "客户端版本", FieldKind::Text)
                .with_default(DefaultValue::Text("131329")),
            Field::new("x_wechat_uin", "X-Wechat-UIN", FieldKind::Text)
                .with_default(DefaultValue::Text("MA==")),
            Field::new("route_tag", "路由标签（可选）", FieldKind::Text).optional(),
        ],
        batch: false,
    },
    Configuration {
        id: "imessage-bluebubble",
        title: "BlueBubbles",
        endpoint: "/api/gateway/bluebubbles",
        description: "连接 BlueBubbles 服务器；替换当前运行时通道。",
        fields: &[
            Field::new("server_url", "服务器 URL", FieldKind::Url),
            Field::new("password", "服务器密码", FieldKind::Password),
            Field::new("use_private_api", "使用 Private API", FieldKind::Boolean)
                .with_default(DefaultValue::Boolean(true)),
            Field::new(
                "stream_edit_min_delta_bytes",
                "流式编辑最小增量（字节）",
                FieldKind::Number,
            )
            .with_default(DefaultValue::Number(128)),
            Field::new("stream_max_edits", "最大流式编辑次数", FieldKind::Number)
                .with_default(DefaultValue::Number(4)),
        ],
        batch: false,
    },
    Configuration {
        id: "imessage-telegram",
        title: "Telegram",
        endpoint: "/api/gateway/telegram",
        description: "配置 Telegram Bot；替换当前运行时通道。",
        fields: &[
            Field::new("token", "Bot Token", FieldKind::Password),
            Field::new("api_base", "API Base URL", FieldKind::Url)
                .with_default(DefaultValue::Text("https://api.telegram.org")),
            Field::new(
                "draft_min_delta_bytes",
                "草稿最小增量（字节）",
                FieldKind::Number,
            )
            .with_default(DefaultValue::Number(24)),
        ],
        batch: false,
    },
    Configuration {
        id: "imessage-inkbox",
        title: "Inkbox",
        endpoint: "/api/gateway/inkbox",
        description: "配置 Inkbox 身份与服务；替换当前运行时通道。",
        fields: &[
            Field::new("api_key", "API Key", FieldKind::Password),
            Field::new("identity_id", "Identity ID", FieldKind::Text),
            Field::new("api_base", "API Base URL", FieldKind::Url)
                .with_default(DefaultValue::Text("https://inkbox.ai")),
        ],
        batch: false,
    },
];

struct Response {
    status: StatusCode,
    body: Bytes,
}

/// Runs the terminal configuration menu for one Barracuda HTTP address.
pub async fn run(address: &str) -> Result<()> {
    println!("Barracuda configuration: {address}");
    println!("Values are sent over plaintext HTTP; use only a trusted configuration network.\n");

    let theme = ColorfulTheme::default();
    loop {
        let mut choices: Vec<String> = CONFIGURATIONS
            .iter()
            .map(|configuration| format!("{}  ({})", configuration.title, configuration.id))
            .collect();
        choices.push(String::from("Done"));
        let selection = Select::with_theme(&theme)
            .with_prompt("Configure Barracuda")
            .items(&choices)
            .default(0)
            .interact()
            .context("select Plugin configuration")?;
        let Some(configuration) = CONFIGURATIONS.get(selection) else {
            return Ok(());
        };

        println!("\n{}\n{}", configuration.title, configuration.description);
        let mut payload = prompt_payload(configuration, &theme)?;
        if configuration.batch {
            payload = Value::Array(vec![payload]);
        }
        let response = post(
            address,
            configuration.endpoint,
            serde_json::to_vec(&payload).context("encode Plugin configuration")?,
        )
        .await?;
        if response.status != StatusCode::NO_CONTENT {
            let detail = String::from_utf8_lossy(&response.body);
            let detail = detail.trim();
            if detail.is_empty() {
                bail!(
                    "{} rejected the configuration with HTTP {}",
                    configuration.title,
                    response.status
                );
            }
            bail!(
                "{} rejected the configuration with HTTP {}: {detail}",
                configuration.title,
                response.status
            );
        }
        println!(
            "Configured {}. The device accepted the values; upstream connectivity was not verified.\n",
            configuration.title
        );
    }
}

fn prompt_payload(configuration: &Configuration, theme: &ColorfulTheme) -> Result<Value> {
    let mut payload = Map::new();
    for field in configuration.fields {
        let value = match field.kind {
            FieldKind::Password => {
                let value = Password::with_theme(theme)
                    .with_prompt(field.label)
                    .allow_empty_password(field.optional)
                    .interact()
                    .with_context(|| format!("read {}", field.label))?;
                optional_value(field, value, |value| Ok(Value::String(value)))?
            }
            FieldKind::Boolean => {
                let default = match field.default {
                    Some(DefaultValue::Boolean(value)) => value,
                    None => false,
                    Some(_) => bail!("invalid boolean default for `{}`", field.name),
                };
                Some(Value::Bool(
                    Confirm::with_theme(theme)
                        .with_prompt(field.label)
                        .default(default)
                        .interact()
                        .with_context(|| format!("read {}", field.label))?,
                ))
            }
            FieldKind::Choice => {
                if field.options.is_empty() {
                    bail!("configuration field `{}` has no choices", field.name);
                }
                let default = match field.default {
                    Some(DefaultValue::Text(value)) => field
                        .options
                        .iter()
                        .position(|option| *option == value)
                        .unwrap_or(0),
                    None => 0,
                    Some(_) => bail!("invalid choice default for `{}`", field.name),
                };
                let selection = Select::with_theme(theme)
                    .with_prompt(field.label)
                    .items(field.options)
                    .default(default)
                    .interact()
                    .with_context(|| format!("read {}", field.label))?;
                Some(Value::String(String::from(field.options[selection])))
            }
            FieldKind::Text | FieldKind::Url | FieldKind::Number => {
                let default = field.default.map(value_as_input).transpose()?;
                let kind = field.kind;
                let optional = field.optional;
                let mut prompt = Input::<String>::with_theme(theme)
                    .with_prompt(field.label)
                    .allow_empty(optional)
                    .validate_with(move |value: &String| validate_input(kind, optional, value));
                if let Some(default) = default {
                    prompt = prompt.default(default);
                }
                let value = prompt
                    .interact_text()
                    .with_context(|| format!("read {}", field.label))?;
                optional_value(field, value, |value| match field.kind {
                    FieldKind::Number => value
                        .parse::<u32>()
                        .map(Value::from)
                        .context("parse unsigned integer"),
                    FieldKind::Text | FieldKind::Url => Ok(Value::String(value)),
                    _ => unreachable!("field kind selected above"),
                })?
            }
        };
        if let Some(value) = value {
            payload.insert(String::from(field.name), value);
        }
    }
    Ok(Value::Object(payload))
}

fn optional_value<F>(field: &Field, value: String, convert: F) -> Result<Option<Value>>
where
    F: FnOnce(String) -> Result<Value>,
{
    if field.optional && value.is_empty() {
        Ok(None)
    } else {
        convert(value).map(Some)
    }
}

fn value_as_input(value: DefaultValue) -> Result<String> {
    match value {
        DefaultValue::Text(value) => Ok(String::from(value)),
        DefaultValue::Number(value) => Ok(value.to_string()),
        DefaultValue::Boolean(_) => bail!("configuration input has an incompatible default value"),
    }
}

fn validate_input(
    kind: FieldKind,
    optional: bool,
    value: &str,
) -> std::result::Result<(), &'static str> {
    if optional && value.is_empty() {
        return Ok(());
    }
    match kind {
        FieldKind::Url if !is_http_url(value) => Err("enter an HTTP or HTTPS URL"),
        FieldKind::Number if value.parse::<u32>().is_err() => {
            Err("enter an integer from 0 to 4294967295")
        }
        _ if value.trim().is_empty() => Err("enter a value"),
        _ => Ok(()),
    }
}

fn is_http_url(value: &str) -> bool {
    let authority = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"));
    authority.is_some_and(|authority| {
        !authority.is_empty() && !value.bytes().any(|byte| byte.is_ascii_whitespace())
    })
}

async fn post(address: &str, path: &str, body: Vec<u8>) -> Result<Response> {
    timeout(REQUEST_TIMEOUT, post_inner(address, path, body))
        .await
        .context("Barracuda configuration request timed out")?
}

async fn post_inner(address: &str, path: &str, body: Vec<u8>) -> Result<Response> {
    if !path.starts_with('/') || path.contains(['?', '#']) {
        bail!("invalid Plugin configuration endpoint `{path}`");
    }
    let uri: Uri = format!("{}{path}", address.trim_end_matches('/'))
        .parse()
        .context("parse Barracuda HTTP address")?;
    if uri.scheme_str() != Some("http") {
        bail!("Barracuda configuration requires an http:// address");
    }
    let host = uri.host().context("Barracuda address has no host")?;
    let port = uri.port_u16().unwrap_or(80);
    let socket_address = if host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_ipv6())
    {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let stream = TcpStream::connect(&socket_address)
        .await
        .with_context(|| format!("connect to Barracuda at {socket_address}"))?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .context("start Barracuda HTTP connection")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let authority = uri
        .authority()
        .context("Barracuda address has no authority")?;
    let request = Request::post(uri.path_and_query().map_or("/", |value| value.as_str()))
        .header(HOST, authority.as_str())
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_LENGTH, body.len())
        .body(Full::new(Bytes::from(body)))
        .context("build Barracuda HTTP request")?;
    let response = sender
        .send_request(request)
        .await
        .context("send Barracuda HTTP request")?;
    collect_response(response).await
}

async fn collect_response(response: hyper::Response<Incoming>) -> Result<Response> {
    let status = response.status();
    let body = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
        .collect()
        .await
        .map_err(|error| anyhow!("read Barracuda HTTP response: {error}"))?
        .to_bytes();
    Ok(Response { status, body })
}

#[cfg(test)]
mod tests {
    use super::{is_http_url, validate_input, FieldKind, CONFIGURATIONS};

    #[test]
    fn configuration_catalog_is_fully_cli_owned() {
        assert_eq!(
            CONFIGURATIONS
                .iter()
                .map(|configuration| configuration.id)
                .collect::<Vec<_>>(),
            [
                "agent",
                "agent-websearch",
                "imessage-qq",
                "imessage-wechat",
                "imessage-bluebubble",
                "imessage-telegram",
                "imessage-inkbox",
            ]
        );
    }

    #[test]
    fn validates_terminal_field_values() {
        assert!(is_http_url("http://device.local:8787"));
        assert!(is_http_url("https://device.example"));
        assert!(!is_http_url("ws://device.local:8787"));
        assert!(validate_input(FieldKind::Number, false, "4294967295").is_ok());
        assert!(validate_input(FieldKind::Number, false, "4294967296").is_err());
        assert!(validate_input(FieldKind::Url, true, "").is_ok());
    }
}
