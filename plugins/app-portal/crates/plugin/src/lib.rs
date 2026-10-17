//! Installs offered apps from the portal: its page lists the apps the
//! Workspace offers and the install waiting for the person, and only a
//! client on the device's setup hotspot, someone at the device, may install
//! or decline it. A Board without a display installs apps this way.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use barracuda_app_manager_plugin::{AppError, AppInfo, AppManager};
use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use barracuda_wifi_plugin::SETUP_AP_ADDRESS;
use core::net::IpAddr;
use serde::{Deserialize, Serialize};

/// `GET`: the offers and the waiting install.
pub const APPS_API_PATH: &str = "/api/apps";
/// `POST {"id"}`: asks to install an offer, which then waits for the person.
pub const REQUEST_API_PATH: &str = "/api/apps/request";
/// `POST {"id", "digest"}`: installs the waiting app, from the hotspot only.
pub const INSTALL_API_PATH: &str = "/api/apps/install";
/// `POST {"id"}`: declines the waiting app, from the hotspot only.
pub const DECLINE_API_PATH: &str = "/api/apps/decline";
const JSON: &str = "application/json";

/// Registers the portal's apps page and its routes.
#[barracuda_plugin::macros::plugin]
pub struct AppPortalPlugin;

impl AppPortalPlugin {
    /// Creates the unregistered page.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AppPortalPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let apps = context.require::<AppManager>("app-manager")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        let entry = WebEntry {
            id: "apps",
            group: WebGroup::Device,
            order: 30,
            title: WebText {
                zh: "应用",
                en: "Apps",
            },
            summary: WebText {
                zh: "安装工作区里提供的应用",
                en: "Install the apps the Workspace offers",
            },
            icon: Some("icon.svg"),
            figure: None,
            module: "entry.js",
        };
        let files = ResourceFiles::from(context.filesystem()?.clone());
        let waiting = Rc::clone(&apps);
        let entry = portal
            .register_with_status(entry, files, move || status(waiting.install_request()))
            .map_err(PluginError::registration)?;
        context.retain(entry);
        let routes: [(&'static str, Route); 4] = [
            (APPS_API_PATH, Route::List),
            (REQUEST_API_PATH, Route::Request),
            (INSTALL_API_PATH, Route::Install),
            (DECLINE_API_PATH, Route::Decline),
        ];
        for (path, route) in routes {
            let endpoint = Apps {
                apps: Rc::clone(&apps),
                route,
            };
            context.retain(
                webserver
                    .serve_http(path, endpoint)
                    .map_err(PluginError::registration)?,
            );
        }
        Ok(())
    }
}

/// The page's status in the portal: whether an install waits.
fn status(waiting: Option<AppInfo>) -> EntryStatus {
    match waiting {
        Some(app) => EntryStatus::new(
            EntryState::Attention,
            WebText {
                zh: "待确认",
                en: "Waiting",
            },
        )
        .with_detail(app.name.as_str()),
        None => EntryStatus::new(
            EntryState::Ready,
            WebText {
                zh: "无待安装",
                en: "Nothing waiting",
            },
        ),
    }
}

/// Whether `request` came from a client on the device's setup hotspot,
/// which only someone at the device can join.
fn on_hotspot(request: &HttpRequest) -> bool {
    request.local_address() == Some(IpAddr::V4(SETUP_AP_ADDRESS))
}

#[derive(Clone, Copy)]
enum Route {
    List,
    Request,
    Install,
    Decline,
}

struct Apps {
    apps: Rc<AppManager>,
    route: Route,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdRequest {
    id: String,
    #[serde(default)]
    digest: Option<String>,
}

/// An offer, as the page lists it.
#[derive(Serialize)]
struct Offer {
    id: String,
    name: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    incompatible: Option<String>,
}

/// The waiting install, as the page asks about it.
#[derive(Serialize)]
struct Waiting {
    id: String,
    name: String,
    version: String,
    digest: Option<String>,
    signer: Option<String>,
    /// What it asks to be allowed, in the install screen's words.
    asks: Vec<String>,
}

#[derive(Serialize)]
struct Listing {
    waiting: Option<Waiting>,
    offers: Vec<Offer>,
}

impl HttpEndpoint for Apps {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match (self.route, request.method()) {
                (Route::List, HttpMethod::Get) => self.list().await,
                (Route::List, _) => error(405, "method", "只能读取"),
                (_, HttpMethod::Post) => {
                    let Ok(body) = serde_json::from_slice::<IdRequest>(request.body()) else {
                        return error(400, "invalid_request", "请求不是 {\"id\"}");
                    };
                    match self.route {
                        Route::Request => self.request(&body.id).await,
                        Route::Install | Route::Decline if !on_hotspot(&request) => error(
                            403,
                            "setup_hotspot_only",
                            "只有连在设备热点上的手机或电脑能确认安装",
                        ),
                        Route::Install => self.install(&body).await,
                        _ => {
                            self.apps.decline_install(&body.id);
                            HttpResponse::new(204, JSON, Vec::new())
                        }
                    }
                }
                _ => error(405, "method", "只能提交"),
            }
        })
    }
}

impl Apps {
    async fn list(&self) -> HttpResponse {
        let offers = self
            .apps
            .list()
            .await
            .installable
            .into_iter()
            .map(|app| Offer {
                id: app.id,
                name: app.name,
                version: app.version,
                incompatible: app.incompatible,
            })
            .collect();
        let waiting = self.apps.install_request().map(|app| Waiting {
            asks: self
                .apps
                .asks(&app)
                .into_iter()
                .map(|(_, line)| line)
                .collect(),
            id: app.id,
            name: app.name,
            version: app.version,
            digest: app.digest,
            signer: app.signer,
        });
        json(200, &Listing { waiting, offers })
    }

    async fn request(&self, id: &str) -> HttpResponse {
        match self.apps.request_install(id).await {
            Ok(_) => HttpResponse::new(204, JSON, Vec::new()),
            Err(AppError::Incompatible) => {
                let reason = self
                    .apps
                    .install_offer(id)
                    .await
                    .ok()
                    .and_then(|app| app.incompatible)
                    .unwrap_or_default();
                error(409, "incompatible", &reason)
            }
            Err(failure) => app_error(failure),
        }
    }

    async fn install(&self, body: &IdRequest) -> HttpResponse {
        // The person approves the install the page showed them.
        let waiting = self.apps.install_request();
        if waiting
            .as_ref()
            .is_none_or(|app| app.id != body.id || app.digest != body.digest)
        {
            return error(409, "changed", "要安装的应用变了，请刷新后再确认");
        }
        match self.apps.install(&body.id).await {
            Ok(app) => json(
                200,
                &Offer {
                    id: app.id,
                    name: app.name,
                    version: app.version,
                    incompatible: None,
                },
            ),
            Err(failure) => app_error(failure),
        }
    }
}

fn json<T: Serialize>(status: u16, value: &T) -> HttpResponse {
    match serde_json::to_vec(value) {
        Ok(body) => HttpResponse::new(status, JSON, body),
        Err(failure) => {
            log::error!("app portal: cannot encode a response: {failure}");
            HttpResponse::new(500, JSON, Vec::from(&br#"{"error":"encoding"}"#[..]))
        }
    }
}

#[derive(Serialize)]
struct Failure<'a> {
    error: &'a str,
    message: &'a str,
}

fn error(status: u16, code: &str, message: &str) -> HttpResponse {
    json(
        status,
        &Failure {
            error: code,
            message,
        },
    )
}

fn app_error(failure: AppError) -> HttpResponse {
    match failure {
        AppError::NotFound => error(404, "not_found", "工作区里没有这个应用"),
        AppError::SystemApp => error(409, "system_app", "这是系统应用"),
        AppError::InvalidBundle => error(422, "invalid_bundle", "应用文件有问题，或签名不对"),
        AppError::Incompatible => error(409, "incompatible", "不适用于此设备"),
        AppError::Changed => error(409, "changed", "要安装的应用变了，请刷新后再确认"),
        _ => error(500, "storage", "安装失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::Ipv4Addr;

    #[test]
    fn only_the_setup_hotspot_confirms() {
        let at = |address: Ipv4Addr| {
            HttpRequest::new(HttpMethod::Post, Vec::new())
                .with_local_address(Some(IpAddr::V4(address)))
        };
        assert!(on_hotspot(&at(SETUP_AP_ADDRESS)));
        assert!(!on_hotspot(&at(Ipv4Addr::new(192, 168, 1, 20))));
        assert!(!on_hotspot(&HttpRequest::new(HttpMethod::Post, Vec::new())));
    }

    #[test]
    fn the_portal_shows_whether_an_install_waits() {
        assert_eq!(status(None).state, EntryState::Ready);
    }
}
