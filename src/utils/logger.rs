use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};
use tracing_subscriber::layer::SubscriberExt;
use time::macros::format_description;
use time::UtcOffset;
use std::fs::{create_dir_all, OpenOptions};
use std::path::Path;
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

    // 单一日志文件层
    let file_layer = if !no_file_log {
        let log_dir = Path::new(LOG_DIR);

        // 创建日志目录
        create_dir_all(log_dir).expect("Unable to create log directory");

        // 打开日志文件以追加方式写入
        let log_file_path = log_dir.join(LOG_FILE);
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path)
            .expect("Unable to open log file");

        Some(fmt::Layer::default()
            .with_writer(move || log_file.try_clone().expect("Failed to clone log file handle"))
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
