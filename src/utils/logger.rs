use std::fs::{create_dir_all, OpenOptions, File};
use std::io::{Write, Result, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};
use time::macros::format_description;
use time::UtcOffset;
use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::fmt::time::OffsetTime;

#[cfg(target_os = "linux")]
const LOG_DIR: &str = "/var/log/";

#[cfg(target_os = "windows")]
const LOG_DIR: &str = "./log/";

const LOG_FILE: &str = "ua4f.log";
/// 日志文件超过 5MB 后进行复写（清空日志）
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024; // 5MB

/// 自定义文件写入器：在写入前检测文件大小，超过阈值则清空文件
struct RotatingFileWriter {
    file: Arc<Mutex<File>>,
    max_size: u64,
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let mut file = self.file.lock().unwrap();
        let metadata = file.metadata()?;
        // 如果当前文件大小加上本次写入内容超过阈值，则清空文件
        if metadata.len() + buf.len() as u64 > self.max_size {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
        }
        file.write(buf)
    }

    fn flush(&mut self) -> Result<()> {
        let mut file = self.file.lock().unwrap();
        file.flush()
    }
}

impl Clone for RotatingFileWriter {
    fn clone(&self) -> Self {
        RotatingFileWriter {
            file: Arc::clone(&self.file),
            max_size: self.max_size,
        }
    }
}

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

    // 单一日志文件层（使用自定义文件写入器实现超过5MB后复写日志文件）
    let file_layer = if !no_file_log {
        let log_dir = Path::new(LOG_DIR);

        // 创建日志目录
        create_dir_all(log_dir).expect("Unable to create log directory");

        // 打开日志文件（以追加方式打开）
        let log_file_path = log_dir.join(LOG_FILE);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path)
            .expect("Unable to open log file");

        // 构造自定义写入器
        let rotating_writer = RotatingFileWriter {
            file: Arc::new(Mutex::new(file)),
            max_size: MAX_LOG_SIZE,
        };

        Some(fmt::Layer::default()
            .with_writer(move || rotating_writer.clone())
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
        .with(file_layer); // 添加文件层（如果启用）

    // 设置全局订阅者
    tracing::subscriber::set_global_default(subscriber)
        .expect("Unable to set global tracing subscriber");
}
