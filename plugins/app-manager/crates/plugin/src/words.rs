//! What the system UI calls things on screen.

use alloc::{format, string::String};
use barracuda_appkit::{AppError, AppExit, AppState, VmRunOutcome};

/// The system icon shown beside what an app asks for: this Plugin's own
/// `notify` has its own, every other package the same one.
pub(crate) fn permission_icon(package: &str) -> &'static str {
    if package == crate::NOTIFY {
        "alert"
    } else {
        "activity"
    }
}

/// Where a running app is in its lifecycle, for the app switcher.
pub(crate) const fn state(state: AppState) -> &'static str {
    match state {
        AppState::Active | AppState::Inactive | AppState::Running => "运行中",
        AppState::Background => "后台",
        AppState::Suspended => "已挂起",
        AppState::Terminating => "正在退出",
    }
}

/// Why an app that was on the display ended.
pub(crate) fn exit(exit: &AppExit) -> &'static str {
    match exit.outcome {
        VmRunOutcome::Cancelled => "没有及时响应，已被结束",
        _ => "运行出错，未保存的内容已丢失",
    }
}

/// Why an app could not be opened.
pub(crate) const fn open_error(error: AppError) -> &'static str {
    match error {
        AppError::NotInstalled => "还没有安装",
        AppError::InvalidBundle | AppError::ViewRejected => "应用文件有问题",
        AppError::Busy => "同时运行的应用太多了",
        AppError::Incompatible => "不适用于此设备",
        AppError::Changed => "应用在确认后被改动了，请重新安装",
        AppError::Storage => "读取存储失败",
        _ => "暂时无法打开",
    }
}

/// The letter an app's icon shows: the first character of its name.
pub(crate) fn glyph(name: &str) -> String {
    name.chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_default()
}

/// `HH:MM` of a UTC timestamp in milliseconds.
pub(crate) fn clock(millis: u64) -> String {
    let minutes = millis / 60_000;
    format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60)
}
