//! Host launcher for the Barracuda Browser Platform.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _result = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_error| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
    host::run().await
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::{
        env, fs,
        net::{Ipv4Addr, SocketAddr},
        path::{Path, PathBuf},
        process::Command,
    };

    use anyhow::{anyhow, bail, Context as _};
    use barracuda_platform_net_gateway::{serve as serve_gateway, GatewayConfig};
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::{TcpListener, TcpStream},
    };
    use wasm_bindgen_cli_support::Bindgen;

    const INDEX: &[u8] = include_bytes!("../../web/index.html");
    const BOOTSTRAP: &[u8] = include_bytes!("../../web/bootstrap.js");
    const WORKER: &[u8] = include_bytes!("../../web/worker.js");
    const WASI: &[u8] = include_bytes!("../../web/wasi_snapshot_preview1.js");

    struct Inputs {
        application: PathBuf,
        workspace: PathBuf,
        target_directory: PathBuf,
        open_browser: bool,
    }

    pub async fn run() -> anyhow::Result<()> {
        let inputs = inputs()?;
        let flash = barracuda_system_image::deploy_selected(&inputs.workspace)
            .map_err(|error| anyhow!(error))?;
        let output = inputs.target_directory.join("barracuda-browser");
        generate_bindings(&inputs.application, &output)?;

        let http_listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .context("bind Browser Platform HTTP server")?;
        let http_address = http_listener
            .local_addr()
            .context("read Browser Platform HTTP address")?;
        let origin = format!("http://{http_address}");
        let gateway_listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .context("bind Browser Platform network gateway")?;
        let gateway_address = gateway_listener
            .local_addr()
            .context("read Browser Platform gateway address")?;
        let gateway_url = format!("ws://{gateway_address}/v1/connect");
        let page_url = format!("{origin}/");

        let gateway = tokio::spawn(async move {
            serve_gateway(gateway_listener, GatewayConfig::browser(origin)).await
        });
        let server = tokio::spawn(serve_http(
            http_listener,
            output,
            PathBuf::from(flash.destination()),
            gateway_url,
        ));

        println!("Barracuda Browser Platform: {page_url}");
        if inputs.open_browser {
            open(&page_url)?;
        }

        tokio::select! {
            result = gateway => result.context("network gateway task stopped")??,
            result = server => result.context("browser HTTP task stopped")??,
            result = tokio::signal::ctrl_c() => result.context("wait for Ctrl-C")?,
        }
        Ok(())
    }

    fn inputs() -> anyhow::Result<Inputs> {
        let mut arguments = env::args_os().skip(1);
        let application = arguments
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("missing wasm application path"))?;
        let workspace = arguments
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("missing workspace path"))?;
        let target_directory = arguments
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("missing Cargo target directory"))?;
        let mut open_browser = true;
        for argument in arguments {
            if argument == "--no-open" {
                open_browser = false;
            } else {
                bail!(
                    "unsupported Browser Platform argument `{}`",
                    argument.to_string_lossy()
                );
            }
        }
        Ok(Inputs {
            application,
            workspace,
            target_directory,
            open_browser,
        })
    }

    fn generate_bindings(application: &Path, output: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(output)
            .with_context(|| format!("create browser output `{}`", output.display()))?;
        Bindgen::new()
            .input_path(application)
            .out_name("barracuda_system")
            .web(true)?
            .generate(output)
            .context("generate browser bindings")?;

        let glue_path = output.join("barracuda_system.js");
        let glue = fs::read_to_string(&glue_path)
            .with_context(|| format!("read generated bindings `{}`", glue_path.display()))?;
        let glue = glue.replace(
            "from \"wasi_snapshot_preview1\"",
            "from \"./wasi_snapshot_preview1.js\"",
        );
        let marker = "    wasm = instance.exports;";
        if !glue.contains(marker) {
            bail!("generated browser bindings do not expose an initialization point");
        }
        let glue = glue.replace(
            marker,
            "    wasm = instance.exports;\n    import1.setMemory(wasm.memory);",
        );
        fs::write(&glue_path, glue)
            .with_context(|| format!("write generated bindings `{}`", glue_path.display()))
    }

    async fn serve_http(
        listener: TcpListener,
        generated: PathBuf,
        flash: PathBuf,
        gateway_url: String,
    ) -> std::io::Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            let generated = generated.clone();
            let flash = flash.clone();
            let gateway_url = gateway_url.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_request(stream, &generated, &flash, &gateway_url).await {
                    eprintln!("Browser Platform HTTP request failed: {error}");
                }
            });
        }
    }

    async fn serve_request(
        mut stream: TcpStream,
        generated: &Path,
        flash: &Path,
        gateway_url: &str,
    ) -> std::io::Result<()> {
        let mut request = [0_u8; 8 * 1024];
        let count = stream.read(&mut request).await?;
        let first_line = String::from_utf8_lossy(&request[..count])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut parts = first_line.split_ascii_whitespace();
        let method = parts.next().unwrap_or_default();
        let path = parts
            .next()
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default();
        if method != "GET" && method != "HEAD" {
            return respond(
                &mut stream,
                405,
                "text/plain",
                b"Method Not Allowed",
                method,
            )
            .await;
        }

        let dynamic;
        let bytes = match path {
            "/" | "/index.html" => INDEX,
            "/bootstrap.js" => BOOTSTRAP,
            "/worker.js" => WORKER,
            "/wasi_snapshot_preview1.js" => WASI,
            "/config.js" => {
                dynamic = format!("export const gatewayUrl = {gateway_url:?};\n").into_bytes();
                &dynamic
            }
            "/barracuda_system.js" => {
                dynamic = fs::read(generated.join("barracuda_system.js"))?;
                &dynamic
            }
            "/barracuda_system_bg.wasm" => {
                dynamic = fs::read(generated.join("barracuda_system_bg.wasm"))?;
                &dynamic
            }
            "/board.flash" => {
                dynamic = fs::read(flash)?;
                &dynamic
            }
            _ => return respond(&mut stream, 404, "text/plain", b"Not Found", method).await,
        };
        respond(&mut stream, 200, content_type(path), bytes, method).await
    }

    async fn respond(
        stream: &mut TcpStream,
        status: u16,
        content_type: &str,
        body: &[u8],
        method: &str,
    ) -> std::io::Result<()> {
        let reason = match status {
            200 => "OK",
            404 => "Not Found",
            405 => "Method Not Allowed",
            _ => "Error",
        };
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nCross-Origin-Opener-Policy: same-origin\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await?;
        if method != "HEAD" {
            stream.write_all(body).await?;
        }
        stream.shutdown().await
    }

    fn content_type(path: &str) -> &'static str {
        if path.ends_with(".js") {
            "text/javascript; charset=utf-8"
        } else if path.ends_with(".wasm") {
            "application/wasm"
        } else if path.ends_with(".flash") {
            "application/octet-stream"
        } else {
            "text/html; charset=utf-8"
        }
    }

    fn open(url: &str) -> anyhow::Result<()> {
        let (program, arguments): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
            ("open", vec![url])
        } else if cfg!(target_os = "windows") {
            ("cmd", vec!["/C", "start", "", url])
        } else {
            ("xdg-open", vec![url])
        };
        Command::new(program)
            .args(arguments)
            .spawn()
            .with_context(|| format!("open Browser Platform URL `{url}`"))?;
        Ok(())
    }
}
