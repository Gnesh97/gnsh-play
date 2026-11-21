use gnsh_chiaki_sys::{
    CHIAKI_LOG_ALL, ChiakiLog, ChiakiLogLevel, ChiakiLogLevel_CHIAKI_LOG_DEBUG,
    ChiakiLogLevel_CHIAKI_LOG_ERROR, ChiakiLogLevel_CHIAKI_LOG_INFO,
    ChiakiLogLevel_CHIAKI_LOG_VERBOSE, ChiakiLogLevel_CHIAKI_LOG_WARNING, chiaki_log_init,
};
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};

/// Heap-pinned libchiaki logger forwarding into `tracing`.
pub struct ChiakiLogger {
    inner: Box<ChiakiLog>,
}

unsafe impl Send for ChiakiLogger {}
unsafe impl Sync for ChiakiLogger {}

impl ChiakiLogger {
    pub fn new(verbose: bool) -> Self {
        let mut inner = Box::new(ChiakiLog::default());
        let mask = if verbose {
            CHIAKI_LOG_ALL
        } else {
            CHIAKI_LOG_ALL & !ChiakiLogLevel_CHIAKI_LOG_VERBOSE
        };
        unsafe {
            chiaki_log_init(
                inner.as_mut(),
                mask,
                Some(log_callback),
                std::ptr::null_mut(),
            );
        }
        Self { inner }
    }

    pub fn as_mut_ptr(&mut self) -> *mut ChiakiLog {
        self.inner.as_mut()
    }
}

unsafe extern "C" fn log_callback(level: ChiakiLogLevel, msg: *const c_char, _user: *mut c_void) {
    if msg.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let trimmed = text.trim_end();
    match level {
        x if x == ChiakiLogLevel_CHIAKI_LOG_ERROR => tracing::error!(target: "chiaki", "{trimmed}"),
        x if x == ChiakiLogLevel_CHIAKI_LOG_WARNING => {
            tracing::warn!(target: "chiaki", "{trimmed}")
        }
        x if x == ChiakiLogLevel_CHIAKI_LOG_INFO => tracing::info!(target: "chiaki", "{trimmed}"),
        x if x == ChiakiLogLevel_CHIAKI_LOG_DEBUG => {
            tracing::debug!(target: "chiaki", "{trimmed}")
        }
        x if x == ChiakiLogLevel_CHIAKI_LOG_VERBOSE => {
            tracing::trace!(target: "chiaki", "{trimmed}")
        }
        _ => tracing::debug!(target: "chiaki", "{trimmed}"),
    }
}
