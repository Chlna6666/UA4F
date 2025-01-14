use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};
use tracing_subscriber::layer::SubscriberExt;
use time::macros::format_description;
use time::UtcOffset;
use std::fs::{create_dir_all};
use std::path::Path;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::time::OffsetTime;


#[cfg(target_os = "linux")]
const LOG_DIR: &str = "/var/log/";

#[cfg(target_os = "windows")]
const LOG_DIR: &str = "./log/";

const LOG_FILE: &str = "ua4f.log";

pub fn init_logger(level: String, no_file_log: bool) {
    let local_offset = UtcOffset::current_local_offset().unwrap_or_else(|_| {
        eprintln!("[Warning] Unable to determine local time offset. Falling back to UTC.");
        UtcOffset::UTC
    });

    let timer = OffsetTime::new(
        local_offset,
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6]"),
    );

    // 控制台层
    let console_layer = fmt::Layer::default()
        .with_writer(std::io::stdout)
        .with_timer(timer.clone())
        .with_ansi(atty::is(atty::Stream::Stdout)) // 仅在交互式终端启用 ANSI 颜色
        .with_target(true) // 显示目标模块
        .with_filter(EnvFilter::new(level.clone()));

    // 滚动日志文件层
    let file_layer = if !no_file_log {
        let log_dir = Path::new(LOG_DIR);

        // 创建日志目录
        create_dir_all(log_dir).expect("Unable to create log directory");

        // 使用 tracing-appender 实现按日滚动日志
        let rolling_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, LOG_FILE);

        Some(fmt::Layer::default()
            .with_writer(rolling_appender)
            .with_timer(timer) // 使用与控制台相同的时间格式
            .with_ansi(false)  // 文件日志不需要颜色
            .with_target(true)
            .with_filter(EnvFilter::new(level)))
    } else {
        None
    };

    // 构建订阅者
    let subscriber = Registry::default()
        .with(console_layer)
        .with(file_layer); // 确保 file_layer 被正确添加

    // 设置全局订阅者
    tracing::subscriber::set_global_default(subscriber)
        .expect("Unable to set global tracing subscriber");
}
