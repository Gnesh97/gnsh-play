#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::all
)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn lib_init_and_controller_idle_work() {
        unsafe {
            let err = chiaki_lib_init();
            assert_eq!(err, ChiakiErrorCode_CHIAKI_ERR_SUCCESS);

            let err_str = CStr::from_ptr(chiaki_error_string(err))
                .to_str()
                .expect("valid UTF-8");
            assert!(!err_str.is_empty());

            let mut state = ChiakiControllerState::default();
            chiaki_controller_state_set_idle(&mut state);
            assert_eq!(state.buttons, 0);
            assert_eq!(state.touches[0].id, -1);
            assert_eq!(state.touches[1].id, -1);
        }
    }
}
