use crate::{ProtoError, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::io::Read as _;
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::HSTRING;

const MAX_PSN_RESPONSE_BYTES: u64 = 64 * 1024;
const PSN_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

pub const PSN_CLIENT_ID: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
pub const PSN_CLIENT_SECRET: &str = "mvaiZkRsAsI1IBkY";
pub const PSN_REDIRECT_URI: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";
pub const PSN_LOGIN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize?service_entity=urn:service-entity:psn&response_type=code&client_id=ba495a24-818c-472b-b12d-ff231c1b5745&redirect_uri=https://remoteplay.dl.playstation.net/remoteplay/redirect&scope=psn:clientapp&request_locale=en_US&ui=pr&service_logo=ps&layout_type=popup&smcid=remoteplay&prompt=always&PlatformPrivacyWs1=minimal&";
pub const PSN_TOKEN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";

pub fn open_psn_login_in_browser() -> Result<()> {
    let op = HSTRING::from("open");
    let url = HSTRING::from(PSN_LOGIN_URL);
    let ret = unsafe { ShellExecuteW(Some(HWND::default()), &op, &url, None, None, SW_SHOWNORMAL) };
    if (ret.0 as isize) <= 32 {
        return Err(ProtoError::Windows(format!(
            "ShellExecuteW failed opening PSN URL (code {})",
            ret.0 as isize
        )));
    }
    Ok(())
}

pub fn extract_code_from_redirect_url(input: &str) -> Result<String> {
    let trimmed = input.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or("");
    let query = without_fragment
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or(without_fragment);

    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == "code"
            && !v.is_empty()
        {
            let decoded = percent_decode(v);
            if decoded.is_empty()
                || decoded.len() > 512
                || !decoded
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '+'))
            {
                return Err(ProtoError::PsnAuth(
                    "redirect URL contains invalid `code` characters".to_string(),
                ));
            }
            return Ok(decoded);
        }
    }
    Err(ProtoError::PsnAuth(
        "redirect URL does not contain a non-empty `code` parameter".to_string(),
    ))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn encode_account_id_u64(user_id: u64) -> String {
    STANDARD.encode(user_id.to_le_bytes())
}

pub fn decode_account_id_b64(input: &str) -> Result<[u8; 8]> {
    let decoded = STANDARD
        .decode(input.trim())
        .map_err(|e| ProtoError::InvalidArg(format!("invalid Base64 PSN Account ID: {e}")))?;
    if decoded.len() != 8 {
        return Err(ProtoError::InvalidArg(format!(
            "PSN Account ID must decode to 8 bytes, got {}",
            decoded.len()
        )));
    }
    let mut out = [0u8; 8];
    out.copy_from_slice(&decoded);
    Ok(out)
}

fn read_capped_body(resp: &mut ureq::http::Response<ureq::Body>) -> Result<String> {
    let mut raw = String::new();
    resp.body_mut()
        .as_reader()
        .take(MAX_PSN_RESPONSE_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|e| ProtoError::PsnAuth(format!("reading response body failed: {e}")))?;
    if raw.len() as u64 > MAX_PSN_RESPONSE_BYTES {
        return Err(ProtoError::PsnAuth(
            "PSN response exceeded 64 KiB safety limit".into(),
        ));
    }
    Ok(raw)
}

fn validate_access_token(token: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > 2048
        || token.contains("..")
        || !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '+' | '/'))
    {
        return Err(ProtoError::PsnAuth(
            "invalid `access_token` format in token response".into(),
        ));
    }
    Ok(())
}

pub fn exchange_redirect_url_for_account_id(redirect_url: &str) -> Result<String> {
    let code = extract_code_from_redirect_url(redirect_url)?;
    let basic_auth = format!(
        "Basic {}",
        STANDARD.encode(format!("{PSN_CLIENT_ID}:{PSN_CLIENT_SECRET}"))
    );
    let body =
        format!("grant_type=authorization_code&code={code}&redirect_uri={PSN_REDIRECT_URI}&");

    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(PSN_HTTP_TIMEOUT))
            .https_only(true)
            .build(),
    );

    let mut token_resp = agent
        .post(PSN_TOKEN_URL)
        .header("Authorization", &basic_auth)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body)
        .map_err(|e| ProtoError::PsnAuth(format!("token request failed: {e}")))?;

    let token_raw = read_capped_body(&mut token_resp)?;
    let token_json: serde_json::Value = serde_json::from_str(&token_raw)
        .map_err(|e| ProtoError::PsnAuth(format!("invalid token JSON: {e}")))?;
    let access_token = token_json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtoError::PsnAuth("missing `access_token` in token response".into()))?;
    validate_access_token(access_token)?;

    let info_url = format!("{PSN_TOKEN_URL}/{access_token}");
    let mut info_resp = agent
        .get(&info_url)
        .header("Authorization", &basic_auth)
        .call()
        .map_err(|e| ProtoError::PsnAuth(format!("account info request failed: {e}")))?;

    let info_raw = read_capped_body(&mut info_resp)?;
    let info_json: serde_json::Value = serde_json::from_str(&info_raw)
        .map_err(|e| ProtoError::PsnAuth(format!("invalid account info JSON: {e}")))?;

    let user_id_val = info_json
        .get("user_id")
        .ok_or_else(|| ProtoError::PsnAuth("missing `user_id` in account info".into()))?;
    let user_id: u64 = if let Some(s) = user_id_val.as_str() {
        s.parse()
            .map_err(|e| ProtoError::PsnAuth(format!("invalid `user_id` string: {e}")))?
    } else if let Some(n) = user_id_val.as_u64() {
        n
    } else {
        return Err(ProtoError::PsnAuth(
            "unsupported `user_id` JSON type".into(),
        ));
    };

    Ok(encode_account_id_u64(user_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_oauth_code_from_redirect_url() {
        let url = "https://remoteplay.dl.playstation.net/remoteplay/redirect?code=v3.AbCd123%2Bxyz&cid=foo#frag";
        let code = extract_code_from_redirect_url(url).expect("code extracted");
        assert_eq!(code, "v3.AbCd123+xyz");

        assert!(extract_code_from_redirect_url("https://example.com/?foo=bar").is_err());
        assert!(
            extract_code_from_redirect_url(
                "https://remoteplay.dl.playstation.net/redirect?code=abc%26grant_type=evil"
            )
            .is_err()
        );
        assert!(
            extract_code_from_redirect_url(
                "https://remoteplay.dl.playstation.net/redirect?code=../../etc/passwd"
            )
            .is_err()
        );
        assert!(
            extract_code_from_redirect_url(
                "https://remoteplay.dl.playstation.net/redirect?code=abc%0d%0aevil"
            )
            .is_err()
        );
        assert!(validate_access_token("../../admin").is_err());
        assert!(validate_access_token("tok?query=1").is_err());
    }

    #[test]
    fn roundtrips_psn_account_id_little_endian_base64() {
        let id = 0x0123_4567_89ab_cdef_u64;
        let b64 = encode_account_id_u64(id);
        let bytes = decode_account_id_b64(&b64).expect("valid 8-byte account id");
        assert_eq!(u64::from_le_bytes(bytes), id);

        assert!(decode_account_id_b64("AQID").is_err());
        assert!(decode_account_id_b64("!!!not-base64!!!").is_err());
    }
}
