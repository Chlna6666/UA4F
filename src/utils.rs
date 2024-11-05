use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config, Root};
use log4rs::encode::pattern::PatternEncoder;

const PATTERN: &str = "{d(%Y-%m-%d %H:%M:%S %Z)} [{h({l})}] {M} - {m}{n}";


#[cfg(target_os = "linux")]
const LOG_PATH: &str = "/var/log/ua4f.log";

#[cfg(target_os = "windows")]
const LOG_PATH: &str = "./log/ua4f.log";

pub fn init_logger(level: String, no_file_log: bool) {
    let log_level = match level.as_str() {
        "debug" => LevelFilter::Debug,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info,
    };

    // Console Appender with pattern encoding
    let stdout = ConsoleAppender::builder()
        .encoder(Box::new(PatternEncoder::new(PATTERN)))
        .build();

    // File Appender with pattern encoding
    let logfile = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(PATTERN)))
        .build(LOG_PATH)
        .expect("Failed to create log file appender");

    // Root configuration based on no_file_log option
    let root = if no_file_log {
        Root::builder().appender("stdout").build(log_level)
    } else {
        Root::builder().appender("stdout").appender("logfile").build(log_level)
    };

    // Config Builder
    let config_builder = Config::builder().appender(Appender::builder().build("stdout", Box::new(stdout)));

    let config = if no_file_log {
        config_builder.build(root).expect("Failed to build config")
    } else {
        config_builder
            .appender(Appender::builder().build("logfile", Box::new(logfile)))
            .build(root)
            .expect("Failed to build config")
    };

    // Initialize log configuration
    log4rs::init_config(config).expect("Failed to initialize logger");
}
