pub mod controller;
pub mod discovery;
pub mod log;
pub mod psn_auth;
pub mod regist;
pub mod session;

use std::ffi::CStr;
use std::sync::OnceLock;
use thiserror::Error;

pub use controller::ControllerState;
pub use discovery::{
    ConsoleState, DiscoveredConsole, discover_ps5_hosts, discover_ps5_lan, normalize_console_host,
    resolve_host_ipv4, wake_ps5,
};
pub use psn_auth::{
    PSN_LOGIN_URL, decode_account_id_b64, encode_account_id_u64,
    exchange_redirect_url_for_account_id, extract_code_from_redirect_url,
    open_psn_login_in_browser,
};
pub use regist::{RegisteredConsole, register_ps5};
pub use session::{
    AudioPcmFrame, HapticsPcmFrame, MAX_BITRATE_KBPS, MIC_CHANNELS, MIC_FRAME_PCM_LEN,
    MIC_FRAME_SAMPLES, MIC_SAMPLE_RATE, MIN_BITRATE_KBPS, Ps5Session, Ps5SessionConfig,
    SessionChannels, SessionEvent, StreamTelemetry, UDP_RECV_BUF_BYTES, VideoCodec, VideoPacket,
    VideoPreset, clamp_bitrate_kbps,
};

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("libchiaki error ({code}): {message}")]
    Chiaki { code: u32, message: String },
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error("operation timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("registration failed or canceled")]
    RegistrationFailed,
    #[error("PSN OAuth error: {0}")]
    PsnAuth(String),
    #[error("Windows API error: {0}")]
    Windows(String),
}

pub type Result<T> = std::result::Result<T, ProtoError>;

static LIB_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Initializes global libchiaki state once per process.
pub fn init_lib() -> Result<()> {
    let res = LIB_INIT.get_or_init(|| unsafe {
        let err = gnsh_chiaki_sys::chiaki_lib_init();
        if err == gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS {
            Ok(())
        } else {
            Err(chiaki_error_text(err))
        }
    });
    res.as_ref().map(|_| ()).map_err(|msg| ProtoError::Chiaki {
        code: 1,
        message: msg.clone(),
    })
}

pub(crate) fn chiaki_error_text(code: gnsh_chiaki_sys::ChiakiErrorCode) -> String {
    unsafe {
        let ptr = gnsh_chiaki_sys::chiaki_error_string(code);
        if ptr.is_null() {
            format!("unknown error {code}")
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

pub(crate) fn check_chiaki(code: gnsh_chiaki_sys::ChiakiErrorCode) -> Result<()> {
    if code == gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS {
        Ok(())
    } else {
        Err(ProtoError::Chiaki {
            code,
            message: chiaki_error_text(code),
        })
    }
}
