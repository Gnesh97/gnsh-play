use std::io::Write as _;
use std::path::PathBuf;
use std::thread;

const MAX_LOG_FILE_BYTES: u64 = 2 * 1024 * 1024;
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
const FILE_TYPE_UNKNOWN: u32 = 0;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn AttachConsole(dw_process_id: u32) -> i32;
    fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
    fn GetFileType(h_file: *mut std::ffi::c_void) -> u32;
}

/// Restores CLI stdout/stderr when invoked from an interactive console in a Windows GUI binary.
pub fn attach_parent_console() {
    // SAFETY: Querying STD_OUTPUT_HANDLE and calling AttachConsole(ATTACH_PARENT_PROCESS)
    // when stdout is unattached preserves redirected pipes/files while restoring interactive CLI output.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let invalid = (-1isize) as *mut std::ffi::c_void;
        if stdout.is_null() || stdout == invalid || GetFileType(stdout) == FILE_TYPE_UNKNOWN {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

struct AsyncLogMakeWriter {
    tx: crossbeam_channel::Sender<Vec<u8>>,
}

struct AsyncLogWriter {
    tx: crossbeam_channel::Sender<Vec<u8>>,
    buf: Vec<u8>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AsyncLogMakeWriter {
    type Writer = AsyncLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        AsyncLogWriter {
            tx: self.tx.clone(),
            buf: Vec::with_capacity(256),
        }
    }
}

impl std::io::Write for AsyncLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
        Ok(())
    }
}

impl Drop for AsyncLogWriter {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
    }
}

fn default_log_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("gnsh-play").join("gnsh-play.log")
}

/// Initializes non-blocking stderr + `%APPDATA%\gnsh-play\gnsh-play.log` tracing and panic capture.
pub fn init_logging() -> PathBuf {
    let log_path = default_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(meta) = std::fs::metadata(&log_path)
        && meta.len() > MAX_LOG_FILE_BYTES
    {
        let _ = std::fs::File::create(&log_path);
    }

    // Offload log I/O off time-critical UDP threads so console/disk latency never stalls Takion receive.
    let (log_tx, log_rx) = crossbeam_channel::bounded::<Vec<u8>>(1024);
    let writer_path = log_path.clone();
    let _ = thread::Builder::new()
        .name("gnsh-log-writer".into())
        .spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&writer_path)
                .ok();
            let mut stderr = std::io::stderr();
            while let Ok(chunk) = log_rx.recv() {
                let _ = stderr.write_all(&chunk);
                if let Some(ref mut f) = file {
                    let _ = f.write_all(&chunk);
                }
            }
        });

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_writer(AsyncLogMakeWriter { tx: log_tx })
        .try_init();

    let panic_path = log_path.clone();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "unknown panic payload"
        };
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        tracing::error!("PANIC at {location}: {payload}");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&panic_path)
        {
            let _ = writeln!(f, "PANIC at {location}: {payload}");
        }
        prev_hook(info);
    }));

    tracing::info!(
        "gnsh-play {} starting; log file: {}",
        env!("CARGO_PKG_VERSION"),
        log_path.display()
    );
    log_path
}
