use crate::discovery::{sanitize_network_text, validate_console_host};
use crate::log::ChiakiLogger;
use crate::psn_auth::decode_account_id_b64;
use crate::{ProtoError, Result, check_chiaki, init_lib};
use gnsh_chiaki_sys::{
    ChiakiRegist, ChiakiRegistEvent, ChiakiRegistInfo, ChiakiTarget_CHIAKI_TARGET_PS5_1,
    chiaki_regist_event_type_t_CHIAKI_REGIST_EVENT_TYPE_FINISHED_SUCCESS, chiaki_regist_fini,
    chiaki_regist_start, chiaki_regist_stop,
};
use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::c_void;
use std::time::Duration;

#[derive(Clone, PartialEq, Eq)]
pub struct RegisteredConsole {
    pub host_name: String,
    pub host_mac: [u8; 6],
    pub addr: String,
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
    pub psn_account_id: String,
}

impl fmt::Debug for RegisteredConsole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredConsole")
            .field("host_name", &self.host_name)
            .field("host_mac", &self.host_mac)
            .field("addr", &self.addr)
            .field("regist_key", &"<redacted>")
            .field("rp_key", &"<redacted>")
            .field("psn_account_id", &self.psn_account_id)
            .finish()
    }
}

impl RegisteredConsole {
    pub fn regist_key_hex(&self) -> String {
        let end = self
            .regist_key
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.regist_key.len());
        String::from_utf8_lossy(&self.regist_key[..end]).into_owned()
    }
}

struct RegistContext {
    tx: crossbeam_channel::Sender<Result<RegisteredConsole>>,
    addr: String,
    psn_account_id: String,
}

unsafe extern "C" fn regist_cb(event: *mut ChiakiRegistEvent, user: *mut c_void) {
    if event.is_null() || user.is_null() {
        return;
    }
    let ctx = unsafe { &*(user.cast::<RegistContext>()) };
    let ev = unsafe { &*event };

    if ev.type_ == chiaki_regist_event_type_t_CHIAKI_REGIST_EVENT_TYPE_FINISHED_SUCCESS
        && !ev.registered_host.is_null()
    {
        let host = unsafe { &*ev.registered_host };
        let raw_nickname = unsafe { CStr::from_ptr(host.server_nickname.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let nickname = sanitize_network_text(&raw_nickname, 128);
        let mut regist_key = [0u8; 16];
        for (dst, &src) in regist_key.iter_mut().zip(host.rp_regist_key.iter()) {
            *dst = src as u8;
        }
        let console = RegisteredConsole {
            host_name: if nickname.is_empty() {
                format!("PS5 ({})", ctx.addr)
            } else {
                nickname
            },
            host_mac: host.server_mac,
            addr: ctx.addr.clone(),
            regist_key,
            rp_key: host.rp_key,
            psn_account_id: ctx.psn_account_id.clone(),
        };
        let _ = ctx.tx.try_send(Ok(console));
    } else {
        let _ = ctx.tx.try_send(Err(ProtoError::RegistrationFailed));
    }
}

/// Registers this client with a PS5 using an 8-digit PIN from PS5 Remote Play settings.
pub fn register_ps5(
    host: &str,
    psn_account_id_b64: &str,
    pin: u32,
    timeout: Duration,
) -> Result<RegisteredConsole> {
    init_lib()?;
    if pin > 99_999_999 {
        return Err(ProtoError::InvalidArg(
            "PS5 PIN must be at most 8 digits".into(),
        ));
    }
    let validated_host = validate_console_host(host)?;
    let account_bytes = decode_account_id_b64(psn_account_id_b64)?;
    let c_host = CString::new(validated_host.as_str())
        .map_err(|_| ProtoError::InvalidArg("host contains interior NUL".into()))?;

    let mut logger = ChiakiLogger::new(false);
    let (tx, rx) = crossbeam_channel::bounded(1);
    let ctx_ptr = Box::into_raw(Box::new(RegistContext {
        tx,
        addr: validated_host,
        psn_account_id: psn_account_id_b64.trim().to_string(),
    }));

    let info = ChiakiRegistInfo {
        target: ChiakiTarget_CHIAKI_TARGET_PS5_1,
        host: c_host.as_ptr(),
        broadcast: false,
        psn_online_id: std::ptr::null(),
        psn_account_id: account_bytes,
        pin,
        ..Default::default()
    };

    let mut regist = Box::new(ChiakiRegist::default());
    let start_err = unsafe {
        chiaki_regist_start(
            regist.as_mut(),
            logger.as_mut_ptr(),
            &info,
            Some(regist_cb),
            ctx_ptr.cast(),
        )
    };
    if start_err != gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS {
        unsafe { drop(Box::from_raw(ctx_ptr)) };
        return check_chiaki(start_err).and(Err(ProtoError::RegistrationFailed));
    }

    let outcome = rx
        .recv_timeout(timeout)
        .unwrap_or(Err(ProtoError::Timeout(timeout)));

    unsafe {
        chiaki_regist_stop(regist.as_mut());
        chiaki_regist_fini(regist.as_mut());
        drop(Box::from_raw(ctx_ptr));
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_pin_before_network() {
        let err = register_ps5(
            "127.0.0.1",
            "AQIDBAUGBwg=",
            100_000_000,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, ProtoError::InvalidArg(_)));
    }
}
