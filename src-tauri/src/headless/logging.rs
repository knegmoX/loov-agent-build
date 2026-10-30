//! 服务器模式的日志：stderr（systemd 收进 journal）+ `~/.loov-agent/logs/loov-serve.log`。
//! 桌面版用 tauri_plugin_log，这里不依赖 Tauri。

use std::io::Write;
use std::sync::Mutex;

struct Logger {
    file: Option<Mutex<std::fs::File>>,
}

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {} {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.target(),
            record.args()
        );
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    let dir = crate::config::get_app_config_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("loov-serve.log"))
        .ok()
        .map(Mutex::new);
    let logger: &'static Logger = Box::leak(Box::new(Logger { file }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
