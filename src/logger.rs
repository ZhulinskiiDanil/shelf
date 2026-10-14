use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::time::SystemTime;

struct FileLogger {
    file: Mutex<Option<File>>,
}

static LOGGER: FileLogger = FileLogger { file: Mutex::new(None) };

pub fn init() {
    let dir = crate::config::app_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let file = OpenOptions::new().create(true).append(true).open(dir.join("shelf.log")).ok();
    *LOGGER.file.lock().unwrap() = file;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Info);
}

pub fn set_level(level: &str) {
    let lvl = level.parse().unwrap_or(LevelFilter::Info);
    log::set_max_level(lvl);
}

impl Log for FileLogger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= log::max_level()
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let ms = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let line = format!("{ms} {:5} {}\n", r.level(), r.args());
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
        #[cfg(debug_assertions)]
        eprint!("{line}");
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.flush();
        }
    }
}
