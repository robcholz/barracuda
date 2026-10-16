//! What the system UI calls things on screen.

use alloc::{format, string::String};
use barracuda_appkit::{AppError, AppExit, AppState, VmRunOutcome};

/// How the person is told what a Lua package lets an app do.
pub(crate) fn permission(package: &str) -> String {
    let words = match package {
        "agent" => "让 Agent 替它做事",
        "analog" => "读取模拟输入",
        "audio" => "使用麦克风和扬声器",
        "camera" => "使用相机",
        "display" => "直接在屏幕上绘制",
        "gpio" => "控制 GPIO 引脚",
        "http" => "联网收发数据",
        "i2c" => "访问 I2C 设备",
        "i2s" => "使用 I2S 音频总线",
        "imu" => "读取姿态传感器",
        "led_strip" => "控制灯带",
        "message_queue" => "收发消息队列",
        "power_monitor" => "读取电量",
        "pwm" => "输出 PWM 信号",
        "spi" => "访问 SPI 设备",
        "systeminfo" => "读取设备信息",
        "uart" => "使用串口",
        "vm-filesystem" => "读写工作区文件",
        "vm-time" => "读取时间",
        "webserver" => "提供网页服务",
        other => return format!("使用 {other}"),
    };
    String::from(words)
}

/// The system icon shown beside a permission.
pub(crate) fn permission_icon(package: &str) -> &'static str {
    match package {
        "http" | "webserver" => "globe",
        "camera" => "image",
        "audio" | "i2s" => "mic",
        "led_strip" => "sun",
        "vm-filesystem" => "hard-drive",
        "agent" => "agent",
        "vm-time" => "timer",
        "systeminfo" => "info",
        _ => "activity",
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
