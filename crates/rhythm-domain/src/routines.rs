//! Calendar routines never start a timer. All evaluation receives explicit time
//! and the current device's IANA zone, making DST and travel behavior testable.
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const INVITATION_WINDOW_MS: i64 = 15 * 60 * 1_000;
pub const MAX_ROUTINES: usize = 100;
pub const MAX_ROUTINE_WINDOWS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RoutineError {
    #[error("Invalid routine: {0}")]
    Invalid(&'static str),
    #[error("The time zone is not a recognized IANA zone: {0}")]
    TimeZone(String),
    #[error("The schedule date is outside the supported range")]
    DateRange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineWindow {
    /// Exactly HH:MM from 00:00 through 23:59.
    pub start: String,
    /// Exclusive end on the same day; 24:00 denotes midnight at the day's end.
    pub end: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineInput {
    pub name: String,
    /// ISO weekday numbers: Monday=1 through Sunday=7.
    pub weekdays: Vec<u8>,
    /// Legacy single-time invitations retain their original fifteen-minute lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_time: Option<String>,
    /// Custom daily windows. Exactly one of windows or local_time must be supplied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RoutineWindow>,
    /// `system` follows the device zone; all other values must be IANA zone names.
    pub timezone: String,
    pub focus_ms: u64,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    #[serde(flatten)]
    pub input: RoutineInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub routine_id: String,
    pub name: String,
    /// Identity uses the local calendar date and window start, not an ambiguous offset.
    pub local_date: String,
    pub window_key: String,
    pub window: Option<RoutineWindow>,
    pub due_at_utc_ms: i64,
    pub expires_at_utc_ms: i64,
    pub focus_ms: u64,
}

impl RoutineInput {
    pub fn validate(&self) -> Result<(), RoutineError> {
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
        {
            return Err(RoutineError::Invalid(
                "use a name of 1–256 bytes without control characters",
            ));
        }
        if self.weekdays.is_empty()
            || self.weekdays.len() > 7
            || self.weekdays.iter().any(|day| !(1..=7).contains(day))
        {
            return Err(RoutineError::Invalid(
                "choose weekdays from Monday (1) through Sunday (7)",
            ));
        }
        let mut days = self.weekdays.clone();
        days.sort_unstable();
        days.dedup();
        if days.len() != self.weekdays.len() {
            return Err(RoutineError::Invalid("weekdays must not repeat"));
        }
        match (&self.local_time, self.windows.is_empty()) {
            (Some(time), true) => {
                parse_minute(time, false)?;
            }
            (None, false) => {
                if self.windows.len() > MAX_ROUTINE_WINDOWS {
                    return Err(RoutineError::Invalid("use at most 12 time windows"));
                }
                let mut ranges = self
                    .windows
                    .iter()
                    .map(|window| {
                        let start = parse_minute(&window.start, false)?;
                        let end = parse_minute(&window.end, true)?;
                        if end <= start {
                            return Err(RoutineError::Invalid(
                                "each end time must be after its start on the same day",
                            ));
                        }
                        Ok((start, end))
                    })
                    .collect::<Result<Vec<_>, RoutineError>>()?;
                ranges.sort_unstable();
                if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
                    return Err(RoutineError::Invalid("time windows must not overlap"));
                }
            }
            _ => {
                return Err(RoutineError::Invalid(
                    "supply daily time windows or a legacy local_time, but not both",
                ));
            }
        }
        if self.timezone != "system" {
            self.zone("UTC")?;
        }
        if self.focus_ms == 0 || self.focus_ms > crate::MAX_PERIOD_MS {
            return Err(RoutineError::Invalid(
                "focus duration must be between 1 millisecond and 24 hours",
            ));
        }
        Ok(())
    }

    fn zone(&self, system_zone: &str) -> Result<Tz, RoutineError> {
        let name = if self.timezone == "system" {
            system_zone
        } else {
            &self.timezone
        };
        name.parse()
            .map_err(|_| RoutineError::TimeZone(name.into()))
    }
}

fn parse_minute(value: &str, allow_midnight: bool) -> Result<u16, RoutineError> {
    if allow_midnight && value == "24:00" {
        return Ok(1_440);
    }
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || [0, 1, 3, 4].iter().any(|i| !bytes[*i].is_ascii_digit())
    {
        return Err(RoutineError::Invalid("time must use HH:MM"));
    }
    let hours = (bytes[0] - b'0') as u16 * 10 + (bytes[1] - b'0') as u16;
    let minutes = (bytes[3] - b'0') as u16 * 10 + (bytes[4] - b'0') as u16;
    if hours > 23 || minutes > 59 {
        return Err(RoutineError::Invalid(
            "use times from 00:00 through 23:59, or 24:00 for an end time",
        ));
    }
    Ok(hours * 60 + minutes)
}

/// Resolve each boundary separately: gaps advance to the first valid local minute,
/// repeated times use the first occurrence. A start on a skipped civil date has
/// no boundary; a 24:00 end advances across a skipped following date so the
/// preceding, valid day's window still ends at the next midnight instant.
fn resolve_boundary(date: NaiveDate, minute: u16, zone: Tz) -> Result<Option<i64>, RoutineError> {
    let date = if minute == 1_440 {
        date.succ_opt().ok_or(RoutineError::DateRange)?
    } else {
        date
    };
    let time = NaiveTime::from_num_seconds_from_midnight_opt((minute % 1_440) as u32 * 60, 0)
        .ok_or(RoutineError::DateRange)?;
    let mut local = date.and_time(time);
    loop {
        if let Some(instant) = zone.from_local_datetime(&local).earliest() {
            return Ok(Some(instant.timestamp_millis()));
        }
        local = local
            .checked_add_signed(Duration::minutes(1))
            .ok_or(RoutineError::DateRange)?;
        if minute != 1_440 && local.date() != date {
            return Ok(None);
        }
    }
}

impl Routine {
    pub fn validate(&self) -> Result<(), RoutineError> {
        if self.id.is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control) {
            return Err(RoutineError::Invalid("routine identifier is invalid"));
        }
        self.input.validate()
    }

    /// Current window (or a fresh legacy invitation), never a backlog.
    pub fn due_invitation(
        &self,
        now_utc_ms: i64,
        system_zone: &str,
    ) -> Result<Option<Invitation>, RoutineError> {
        self.validate()?;
        if !self.input.enabled {
            return Ok(None);
        }
        let zone = self.input.zone(system_zone)?;
        let now = DateTime::from_timestamp_millis(now_utc_ms).ok_or(RoutineError::DateRange)?;
        let today = now.with_timezone(&zone).date_naive();
        // Legacy invitations can extend fifteen minutes beyond midnight.
        for days_ago in [0, 1] {
            let date = today
                .checked_sub_signed(Duration::days(days_ago))
                .ok_or(RoutineError::DateRange)?;
            for invitation in self.on_date(date, zone)? {
                if invitation.due_at_utc_ms <= now_utc_ms
                    && now_utc_ms < invitation.expires_at_utc_ms
                {
                    return Ok(Some(invitation));
                }
            }
        }
        Ok(None)
    }

    /// First strictly future start, including another window on the same date.
    pub fn next_invitation(
        &self,
        now_utc_ms: i64,
        system_zone: &str,
    ) -> Result<Option<Invitation>, RoutineError> {
        self.validate()?;
        if !self.input.enabled {
            return Ok(None);
        }
        let zone = self.input.zone(system_zone)?;
        let now = DateTime::from_timestamp_millis(now_utc_ms).ok_or(RoutineError::DateRange)?;
        let today = now.with_timezone(&zone).date_naive();
        // A skipped civil date (date-line change) can suppress one week's occurrence.
        for offset in 0..15 {
            let date = today
                .checked_add_signed(Duration::days(offset))
                .ok_or(RoutineError::DateRange)?;
            for invitation in self.on_date(date, zone)? {
                if invitation.due_at_utc_ms > now_utc_ms {
                    return Ok(Some(invitation));
                }
            }
        }
        Ok(None)
    }

    fn on_date(&self, date: NaiveDate, zone: Tz) -> Result<Vec<Invitation>, RoutineError> {
        if !self
            .input
            .weekdays
            .contains(&(date.weekday().number_from_monday() as u8))
        {
            return Ok(Vec::new());
        }
        let mut invitations = Vec::new();
        if let Some(time) = &self.input.local_time {
            if let Some(start) = resolve_boundary(date, parse_minute(time, false)?, zone)? {
                invitations.push(
                    self.invitation(
                        date,
                        "legacy".into(),
                        None,
                        start,
                        start
                            .checked_add(INVITATION_WINDOW_MS)
                            .ok_or(RoutineError::DateRange)?,
                    ),
                );
            }
        } else {
            for window in &self.input.windows {
                let Some(start) =
                    resolve_boundary(date, parse_minute(&window.start, false)?, zone)?
                else {
                    continue;
                };
                let Some(end) = resolve_boundary(date, parse_minute(&window.end, true)?, zone)?
                else {
                    continue;
                };
                // Both boundaries in a DST gap can resolve to the same instant.
                if end > start {
                    invitations.push(self.invitation(
                        date,
                        window.start.clone(),
                        Some(window.clone()),
                        start,
                        end,
                    ));
                }
            }
        }
        invitations.sort_by_key(|item| item.due_at_utc_ms);
        Ok(invitations)
    }

    fn invitation(
        &self,
        date: NaiveDate,
        window_key: String,
        window: Option<RoutineWindow>,
        due_at_utc_ms: i64,
        expires_at_utc_ms: i64,
    ) -> Invitation {
        Invitation {
            routine_id: self.id.clone(),
            name: self.input.name.clone(),
            local_date: date.to_string(),
            window_key,
            window,
            due_at_utc_ms,
            expires_at_utc_ms,
            focus_ms: self.input.focus_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }
    fn routine(time: &str, days: Vec<u8>) -> Routine {
        Routine {
            id: "routine".into(),
            input: RoutineInput {
                name: "A little focus".into(),
                weekdays: days,
                local_time: Some(time.into()),
                windows: vec![],
                timezone: "America/Los_Angeles".into(),
                focus_ms: 1_500_000,
                enabled: true,
            },
        }
    }
    #[test]
    fn weekday_and_expiry_rules_skip_backlogs() {
        let routine = routine("09:00", vec![1, 2, 3, 4, 5]);
        assert!(
            routine
                .due_invitation(ms("2026-11-02T16:59:59Z"), "UTC")
                .unwrap()
                .is_none()
        );
        assert!(
            routine
                .due_invitation(ms("2026-11-02T17:14:59.999Z"), "UTC")
                .unwrap()
                .is_some()
        );
        assert!(
            routine
                .due_invitation(ms("2026-11-02T17:15:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
        assert!(
            routine
                .due_invitation(ms("2026-11-02T17:15:00.001Z"), "UTC")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            routine
                .next_invitation(ms("2026-11-06T18:00:00Z"), "UTC")
                .unwrap()
                .unwrap()
                .due_at_utc_ms,
            ms("2026-11-09T17:00:00Z")
        );
    }
    #[test]
    fn spring_gap_moves_to_first_valid_minute_and_fall_uses_first_occurrence() {
        let spring = routine("02:30", vec![7])
            .next_invitation(ms("2026-03-08T00:00:00Z"), "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(spring.due_at_utc_ms, ms("2026-03-08T10:00:00Z"));
        let fall = routine("01:30", vec![7]);
        assert_eq!(
            fall.next_invitation(ms("2026-11-01T00:00:00Z"), "UTC")
                .unwrap()
                .unwrap()
                .due_at_utc_ms,
            ms("2026-11-01T08:30:00Z")
        );
        assert!(
            fall.due_invitation(ms("2026-11-01T09:30:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn system_zone_tracks_travel_fixed_zone_does_not() {
        let mut routine = routine("09:00", vec![1]);
        routine.input.timezone = "system".into();
        let now = ms("2026-11-02T00:00:00Z");
        assert_eq!(
            routine
                .next_invitation(now, "America/New_York")
                .unwrap()
                .unwrap()
                .due_at_utc_ms,
            ms("2026-11-02T14:00:00Z")
        );
        assert_eq!(
            routine
                .next_invitation(now, "Asia/Shanghai")
                .unwrap()
                .unwrap()
                .due_at_utc_ms,
            ms("2026-11-02T01:00:00Z")
        );
        assert!(routine.next_invitation(now, "not-a-zone").is_err());
    }
    #[test]
    fn midnight_window_and_disabled_routines() {
        let mut routine = routine("23:55", vec![1]);
        assert_eq!(
            routine
                .due_invitation(ms("2026-11-03T08:05:00Z"), "UTC")
                .unwrap()
                .unwrap()
                .local_date,
            "2026-11-02"
        );
        routine.input.enabled = false;
        assert!(
            routine
                .due_invitation(ms("2026-11-03T08:05:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
        assert!(
            routine
                .next_invitation(ms("2026-11-03T08:05:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn invalid_rules_are_rejected() {
        let mut routine = routine("09:00", vec![1]);
        routine.input.weekdays.push(1);
        assert!(routine.validate().is_err());
        routine.input.weekdays = vec![1];
        routine.input.local_time = Some("9:00".into());
        assert!(routine.validate().is_err());
        routine.input.local_time = Some("24:00".into());
        assert!(routine.validate().is_err());
        routine.input.local_time = Some("09:00".into());
        routine.input.timezone = "PST".into();
        assert!(routine.validate().is_err());
    }
    #[test]
    fn serialized_routine_uses_flat_fields_and_round_trips() {
        let routine = routine("09:00", vec![1]);
        let json = serde_json::to_value(&routine).unwrap();
        assert_eq!(json["local_time"], "09:00");
        assert_eq!(json["id"], "routine");
        assert_eq!(serde_json::from_value::<Routine>(json).unwrap(), routine);
    }

    fn windows(ranges: &[(&str, &str)]) -> Routine {
        let mut routine = routine("09:00", vec![1, 2, 3, 4, 5]);
        routine.input.local_time = None;
        routine.input.timezone = "UTC".into();
        routine.input.windows = ranges
            .iter()
            .map(|(start, end)| RoutineWindow {
                start: (*start).into(),
                end: (*end).into(),
            })
            .collect();
        routine
    }

    #[test]
    fn split_weekday_windows_have_exact_boundaries_and_sorted_previews() {
        let routine = windows(&[("14:30", "18:00"), ("08:30", "12:00")]);
        let at = |time: &str| routine.due_invitation(ms(time), "UTC").unwrap();
        assert!(at("2026-11-02T08:29:59.999Z").is_none());
        let morning = at("2026-11-02T08:30:00Z").unwrap();
        assert_eq!(morning.window_key, "08:30");
        assert_eq!(morning.expires_at_utc_ms, ms("2026-11-02T12:00:00Z"));
        assert!(at("2026-11-02T11:59:59.999Z").is_some());
        assert!(at("2026-11-02T12:00:00Z").is_none());
        assert!(at("2026-11-02T14:29:59.999Z").is_none());
        assert_eq!(at("2026-11-02T14:30:00Z").unwrap().window_key, "14:30");
        assert!(at("2026-11-02T18:00:00Z").is_none());
        assert!(at("2026-11-07T09:00:00Z").is_none());
        let next = |time: &str| routine.next_invitation(ms(time), "UTC").unwrap().unwrap();
        assert_eq!(
            next("2026-11-02T00:00:00Z").due_at_utc_ms,
            morning.due_at_utc_ms
        );
        assert_eq!(
            next("2026-11-02T08:30:00Z").due_at_utc_ms,
            ms("2026-11-02T14:30:00Z")
        );
        assert_eq!(
            next("2026-11-06T18:00:00Z").due_at_utc_ms,
            ms("2026-11-09T08:30:00Z")
        );
    }

    #[test]
    fn midnight_end_is_exclusive_and_adjacent_windows_are_valid() {
        let routine = windows(&[("22:00", "23:00"), ("23:00", "24:00")]);
        assert!(routine.validate().is_ok());
        let at_boundary = routine
            .due_invitation(ms("2026-11-02T23:00:00Z"), "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(at_boundary.window_key, "23:00");
        assert_eq!(at_boundary.expires_at_utc_ms, ms("2026-11-03T00:00:00Z"));
        assert!(
            routine
                .due_invitation(ms("2026-11-03T00:00:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn custom_window_validation_rejects_empty_overlapping_and_overnight_ranges() {
        for ranges in [
            vec![],
            vec![("08:30", "08:30")],
            vec![("18:00", "08:30")],
            vec![("08:30", "12:00"), ("11:00", "13:00")],
            vec![("08:30", "12:00"), ("08:30", "12:00")],
            vec![("24:00", "24:00")],
            vec![("08:30", "24:01")],
            vec![("8:30", "12:00")],
        ] {
            assert!(windows(&ranges).validate().is_err(), "accepted {ranges:?}");
        }
        let mut mixed = windows(&[("08:30", "12:00")]);
        mixed.input.local_time = Some("08:30".into());
        assert!(mixed.validate().is_err());
        let mut excessive = windows(&[("08:30", "12:00")]);
        excessive.input.windows = vec![excessive.input.windows[0].clone(); MAX_ROUTINE_WINDOWS + 1];
        assert!(excessive.validate().is_err());
    }

    #[test]
    fn window_boundaries_resolve_dst_independently_and_collapsed_ranges_are_skipped() {
        let mut routine = windows(&[("01:30", "03:30")]);
        routine.input.timezone = "America/Los_Angeles".into();
        routine.input.weekdays = vec![7];
        let spring = routine
            .next_invitation(ms("2026-03-08T00:00:00Z"), "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(spring.due_at_utc_ms, ms("2026-03-08T09:30:00Z"));
        assert_eq!(spring.expires_at_utc_ms, ms("2026-03-08T10:30:00Z"));
        routine.input.windows = vec![RoutineWindow {
            start: "02:30".into(),
            end: "02:45".into(),
        }];
        assert!(
            routine
                .due_invitation(ms("2026-03-08T10:00:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            routine
                .next_invitation(ms("2026-03-08T00:00:00Z"), "UTC")
                .unwrap()
                .unwrap()
                .local_date,
            "2026-03-15"
        );
        routine.input.windows = vec![RoutineWindow {
            start: "01:30".into(),
            end: "02:30".into(),
        }];
        let fall = routine
            .next_invitation(ms("2026-11-01T00:00:00Z"), "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(fall.due_at_utc_ms, ms("2026-11-01T08:30:00Z"));
        assert_eq!(fall.expires_at_utc_ms, ms("2026-11-01T10:30:00Z"));
        assert_eq!(
            routine
                .due_invitation(ms("2026-11-01T09:30:00Z"), "UTC")
                .unwrap()
                .unwrap(),
            fall
        );
    }

    #[test]
    fn skipped_civil_date_has_no_window() {
        let mut routine = windows(&[("08:30", "12:00")]);
        routine.input.timezone = "Pacific/Apia".into();
        routine.input.weekdays = vec![5];
        let next = routine
            .next_invitation(ms("2011-12-29T00:00:00Z"), "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(next.local_date, "2012-01-06");
    }

    #[test]
    fn midnight_end_survives_a_skipped_following_civil_date() {
        let mut routine = windows(&[("08:30", "24:00")]);
        routine.input.timezone = "Pacific/Apia".into();
        routine.input.weekdays = vec![4];
        let next = routine
            .next_invitation(ms("2011-12-29T12:00:00Z"), "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(next.local_date, "2011-12-29");
        assert_eq!(next.due_at_utc_ms, ms("2011-12-29T18:30:00Z"));
        assert_eq!(next.expires_at_utc_ms, ms("2011-12-30T10:00:00Z"));
        assert!(
            routine
                .due_invitation(ms("2011-12-30T09:59:59Z"), "UTC")
                .unwrap()
                .is_some()
        );
        assert!(
            routine
                .due_invitation(ms("2011-12-30T10:00:00Z"), "UTC")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_and_custom_json_remain_distinct_and_round_trip() {
        let legacy: RoutineInput = serde_json::from_str(r#"{"name":"Legacy","weekdays":[1],"local_time":"23:55","timezone":"UTC","focus_ms":1500000,"enabled":true}"#).unwrap();
        assert!(legacy.validate().is_ok());
        assert!(legacy.windows.is_empty());
        assert_eq!(legacy.local_time.as_deref(), Some("23:55"));
        let routine = windows(&[("08:30", "12:00"), ("14:30", "18:00")]);
        let json = serde_json::to_value(&routine).unwrap();
        assert!(json.get("local_time").is_none());
        assert_eq!(json["windows"][1]["end"], "18:00");
        assert_eq!(serde_json::from_value::<Routine>(json).unwrap(), routine);
    }
}
