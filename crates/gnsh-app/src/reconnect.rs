use std::time::{Duration, Instant};

pub(crate) const MAX_RECONNECT_ATTEMPTS: u8 = 3;
pub(crate) const BACKOFF_SECS: [u64; 3] = [2, 4, 8];
const WAN_FALLBACK_DELAY_SECS: u64 = 1;

const NON_RETRYABLE_MARKERS: &[&str] = &[
    "pin",
    "in use",
    "already",
    "invalid",
    "unregistered",
    "regist",
    "standby",
    "not ready",
    "decoder",
];

/// Returns `true` when a stream disconnect error is likely transient and safe to auto-retry.
pub(crate) fn is_retryable_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    !NON_RETRYABLE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

#[derive(Debug, Clone)]
pub(crate) struct ReconnectState {
    pub(crate) saved_idx: usize,
    pub(crate) console_name: String,
    pub(crate) use_wan: bool,
    pub(crate) can_fallback_wan: bool,
    pub(crate) fell_back_to_wan: bool,
    pub(crate) attempt: u8,
    pub(crate) last_reason: String,
    pub(crate) next_attempt_at: Instant,
}

impl ReconnectState {
    pub(crate) fn start(
        saved_idx: usize,
        console_name: String,
        use_wan: bool,
        has_remote_host: bool,
        reason: String,
        now: Instant,
    ) -> Self {
        Self {
            saved_idx,
            console_name,
            use_wan,
            can_fallback_wan: !use_wan && has_remote_host,
            fell_back_to_wan: false,
            attempt: 1,
            last_reason: reason,
            next_attempt_at: now + Self::backoff_for_attempt(1),
        }
    }

    pub(crate) fn backoff_for_attempt(attempt: u8) -> Duration {
        let idx = usize::from(attempt.saturating_sub(1)).min(BACKOFF_SECS.len() - 1);
        Duration::from_secs(BACKOFF_SECS[idx])
    }

    /// Advances attempt counter or switches LAN to WAN fallback; returns `false` when exhausted.
    pub(crate) fn advance_after_failure(&mut self, reason: String, now: Instant) -> bool {
        self.last_reason = reason;
        if self.attempt < MAX_RECONNECT_ATTEMPTS {
            self.attempt += 1;
            self.next_attempt_at = now + Self::backoff_for_attempt(self.attempt);
            true
        } else if self.can_fallback_wan && !self.fell_back_to_wan {
            self.use_wan = true;
            self.fell_back_to_wan = true;
            self.attempt = 1;
            self.next_attempt_at = now + Duration::from_secs(WAN_FALLBACK_DELAY_SECS);
            true
        } else {
            false
        }
    }

    pub(crate) fn is_due(&self, now: Instant) -> bool {
        now >= self.next_attempt_at
    }

    pub(crate) fn remaining_secs(&self, now: Instant) -> u64 {
        let rem = self.next_attempt_at.saturating_duration_since(now);
        if rem.is_zero() {
            0
        } else {
            rem.as_secs() + u64::from(rem.subsec_nanos() > 0)
        }
    }

    pub(crate) fn status_line(&self, now: Instant) -> String {
        let route = if self.fell_back_to_wan {
            "WAN fallback"
        } else if self.use_wan {
            "WAN"
        } else {
            "LAN"
        };
        let rem = self.remaining_secs(now);
        if rem == 0 {
            format!(
                "Reconnecting to {} ({}, attempt {}/{})...",
                self.console_name, route, self.attempt, MAX_RECONNECT_ATTEMPTS
            )
        } else {
            format!(
                "Reconnecting to {} ({}, attempt {}/{}) in {}s...",
                self.console_name, route, self.attempt, MAX_RECONNECT_ATTEMPTS, rem
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_reason_filters_auth_conflict_and_decoder_errors() {
        assert!(is_retryable_reason("Ctrl connection reset by peer"));
        assert!(is_retryable_reason("Stream timeout after 5s"));

        assert!(!is_retryable_reason("Incorrect login PIN entered"));
        assert!(!is_retryable_reason("Remote Play is already in use"));
        assert!(!is_retryable_reason("Console is in STANDBY"));
        assert!(!is_retryable_reason("Console is not ready"));
        assert!(!is_retryable_reason("Console unregistered"));
        assert!(!is_retryable_reason("Invalid registration key"));
        assert!(!is_retryable_reason("Video decoder initialization failed"));
    }

    #[test]
    fn backoff_follows_2_4_8_and_exhausts_without_wan_fallback() {
        let t0 = Instant::now();
        let mut rc = ReconnectState::start(
            0,
            "PS5-406".into(),
            false,
            false,
            "socket closed".into(),
            t0,
        );
        assert_eq!(rc.attempt, 1);
        assert!(!rc.use_wan);
        assert_eq!(rc.remaining_secs(t0), 2);
        assert!(!rc.is_due(t0 + Duration::from_secs(1)));
        assert!(rc.is_due(t0 + Duration::from_secs(2)));

        let t1 = t0 + Duration::from_secs(2);
        assert!(rc.advance_after_failure("timeout".into(), t1));
        assert_eq!(rc.attempt, 2);
        assert_eq!(rc.remaining_secs(t1), 4);

        let t2 = t1 + Duration::from_secs(4);
        assert!(rc.advance_after_failure("timeout".into(), t2));
        assert_eq!(rc.attempt, 3);
        assert_eq!(rc.remaining_secs(t2), 8);

        let t3 = t2 + Duration::from_secs(8);
        assert!(!rc.advance_after_failure("unreachable".into(), t3));
    }

    #[test]
    fn lan_retries_fall_back_to_wan_and_exhaust_cleanly() {
        let t0 = Instant::now();
        let mut rc = ReconnectState::start(
            1,
            "LivingRoom".into(),
            false,
            true,
            "connection lost".into(),
            t0,
        );
        assert!(rc.advance_after_failure("lan 1".into(), t0));
        assert!(rc.advance_after_failure("lan 2".into(), t0));
        assert!(!rc.use_wan);

        // 3rd LAN failure triggers WAN fallback (attempt resets to 1 after 1s).
        assert!(rc.advance_after_failure("lan 3".into(), t0));
        assert!(rc.use_wan);
        assert!(rc.fell_back_to_wan);
        assert_eq!(rc.attempt, 1);
        assert_eq!(rc.remaining_secs(t0), 1);
        assert!(rc.status_line(t0).contains("WAN fallback, attempt 1/3"));

        // 2 more WAN attempts (attempt 2 & 3), then 4th total WAN failure exhausts.
        assert!(rc.advance_after_failure("wan 1".into(), t0));
        assert!(rc.advance_after_failure("wan 2".into(), t0));
        assert!(!rc.advance_after_failure("wan 3".into(), t0));
        assert_eq!(rc.last_reason, "wan 3");
    }

    #[test]
    fn remaining_secs_rounds_subsecond_duration_up_and_clamps_to_zero() {
        let t0 = Instant::now();
        let rc = ReconnectState::start(0, "PS5".into(), true, true, "lost".into(), t0);
        assert_eq!(rc.remaining_secs(t0 + Duration::from_millis(100)), 2);
        assert_eq!(rc.remaining_secs(t0 + Duration::from_millis(1500)), 1);
        assert_eq!(rc.remaining_secs(t0 + Duration::from_secs(2)), 0);
        assert_eq!(rc.remaining_secs(t0 + Duration::from_secs(5)), 0);
        assert!(
            rc.status_line(t0 + Duration::from_secs(2))
                .contains("(WAN, attempt 1/3)...")
        );
    }
}
