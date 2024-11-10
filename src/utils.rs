use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};
use tracing_subscriber::layer::SubscriberExt;
use std::fs::{create_dir_all, OpenOptions};
use std::path::Path;
use tracing_subscriber::fmt::time::LocalTime;

#[cfg(target_os = "linux")]
const LOG_PATH: &str = "/var/log/ua4f.log";

#[cfg(target_os = "windows")]
const LOG_PATH: &str = "./log/ua4f.log";

pub fn init_logger(level: String, no_file_log: bool) {
    // 控制台层
    let console_layer = fmt::Layer::default()
        .with_writer(std::io::stdout)
        .with_timer(LocalTime::rfc_3339())  // 使用系统本地时间，格式为RFC 3339
        .with_ansi(true)  // 控制台层保留颜色
        .with_filter(EnvFilter::new(level.clone()));

    // 文件层
    let file_layer = if !no_file_log {
        // 获取日志文件路径的目录
        if let Some(log_dir) = Path::new(LOG_PATH).parent() {
            // 如果目录不存在则创建
            create_dir_all(log_dir).expect("无法创建日志文件目录");
        }

        let file_writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_PATH)
            .expect("无法打开日志文件");

        Some(fmt::Layer::default()
            .with_writer(file_writer)
            .with_timer(LocalTime::rfc_3339())  // 使用系统本地时间
            .with_ansi(false)  // 文件层取消颜色
            .with_filter(EnvFilter::new(level)))
    } else {
        None
    };

    // 构建组合后的订阅者
    let subscriber = Registry::default()
        .with(console_layer)
        .with(file_layer);  // 确保 file_layer 被添加

    tracing::subscriber::set_global_default(subscriber)
        .expect("无法设置全局默认日志订阅者");
}
