pub use log::*;

pub struct SimpleLogger;

impl Log for SimpleLogger {
  fn enabled(&self, metadata: &Metadata) -> bool {
    metadata.level() <= max_level()
  }

  fn log(&self, record: &Record) {
    if self.enabled(record.metadata()) {
      if record.level() > Level::Info && record.file().is_some() {
        println!(
          "[{}] {}: {}",
          record.level(),
          record.file().unwrap(),
          record.args()
        );
      } else {
        println!("[{}]: {}", record.level(), record.args());
      }
    }
  }

  fn flush(&self) {}
}
