//! Development server and static bundle exporter for the Browser Platform.

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
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::{TcpListener, TcpStream},
    };

    const INDEX: &[u8] = include_bytes!("../../web/index.html");
    const BOOTSTRAP: &[u8] = include_bytes!("../../web/bootstrap.js");
    const WORKER: &[u8] = include_bytes!("../../web/worker.js");
    const BROWSER_HOST: &[u8] = include_bytes!("../../web/browser_host.js");
    const BROWSER_WEBSOCKET: &[u8] = include_bytes!("../../web/browser_websocket.js");
    const SERVICE_WORKER: &[u8] = include_bytes!("../../web/service-worker.js");
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
        let bundle = inputs.target_directory.join("barracuda-browser");
        export_bundle(&inputs.application, Path::new(flash.destination()), &bundle)?;

        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .context("bind Browser Platform development server")?;
        let address = listener
            .local_addr()
            .context("read Browser Platform development server address")?;
        let page_url = format!("http://{address}/");
        let server = tokio::spawn(serve_http(listener, bundle.clone()));

        println!("Barracuda Browser bundle: {}", bundle.display());
        println!("Barracuda Browser Platform: {page_url}");
        if inputs.open_browser {
            open(&page_url)?;
        }

        tokio::select! {
            result = server => result.context("browser development server stopped")??,
            result = tokio::signal::ctrl_c() => result.context("wait for Ctrl-C")?,
        }
        Ok(())
    }

    fn inputs() -> anyhow::Result<Inputs> {
        let mut arguments = env::args_os().skip(1);
        let application = arguments
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("missing WASI application path"))?;
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

    fn export_bundle(application: &Path, flash: &Path, output: &Path) -> anyhow::Result<()> {
        if output.exists() {
            fs::remove_dir_all(output)
                .with_context(|| format!("remove stale Browser bundle `{}`", output.display()))?;
        }
        fs::create_dir_all(output)
            .with_context(|| format!("create Browser bundle `{}`", output.display()))?;
        for (name, bytes) in [
            ("index.html", INDEX),
            ("bootstrap.js", BOOTSTRAP),
            ("worker.js", WORKER),
            ("browser_host.js", BROWSER_HOST),
            ("browser_websocket.js", BROWSER_WEBSOCKET),
            ("service-worker.js", SERVICE_WORKER),
            ("wasi_snapshot_preview1.js", WASI),
        ] {
            fs::write(output.join(name), bytes)
                .with_context(|| format!("write Browser bundle asset `{name}`"))?;
        }
        fs::copy(application, output.join("barracuda_system.wasm"))
            .context("copy WASI System into Browser bundle")?;
        fs::copy(flash, output.join("board.flash"))
            .context("copy System image into Browser bundle")?;
        Ok(())
    }

    async fn serve_http(listener: TcpListener, bundle: PathBuf) -> std::io::Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            let bundle = bundle.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_request(stream, &bundle).await {
                    eprintln!("Browser Platform HTTP request failed: {error}");
                }
            });
        }
    }

    async fn serve_request(mut stream: TcpStream, bundle: &Path) -> std::io::Result<()> {
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

        let name = match path {
            "/" | "/index.html" => "index.html",
            "/bootstrap.js" => "bootstrap.js",
            "/worker.js" => "worker.js",
            "/browser_host.js" => "browser_host.js",
            "/browser_websocket.js" => "browser_websocket.js",
            "/service-worker.js" => "service-worker.js",
            "/wasi_snapshot_preview1.js" => "wasi_snapshot_preview1.js",
            "/barracuda_system.wasm" => "barracuda_system.wasm",
            "/board.flash" => "board.flash",
            _ => return respond(&mut stream, 404, "text/plain", b"Not Found", method).await,
        };
        let bytes = fs::read(bundle.join(name))?;
        respond(&mut stream, 200, content_type(name), &bytes, method).await
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
