use std::{
    fmt,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Datelike, Local, Timelike, Weekday};
use ruscello_platform::power::QueueCompletionAction;
use serde::{Deserialize, Serialize};

pub(crate) const MINUTES_PER_DAY: u16 = 24 * 60;
const DEFAULT_COUNTDOWN_SECS: u64 = 30;
const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 60;
const DEFAULT_WATCH_POLL_SECS: u64 = 5;

/// Time source abstraction so scheduling logic is deterministic in tests.
pub trait Clock: Send + Sync {
    fn local_now(&self) -> DateTime<Local>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn local_now(&self) -> DateTime<Local> {
        Local::now()
    }
}

/// Seven-day bitmask starting at Monday (bit 0 = Monday .. bit 6 = Sunday).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WeekdayMask(pub u8);

impl WeekdayMask {
    pub const EMPTY: Self = Self(0);
    pub const EVERY_DAY: Self = Self(0b0111_1111);
    pub const WEEKDAYS: Self = Self(0b0001_1111);
    pub const WEEKENDS: Self = Self(0b0110_0000);

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b0111_1111)
    }

    pub const fn bits(self) -> u8 {
        self.0 & 0b0111_1111
    }

    pub fn contains(self, weekday: Weekday) -> bool {
        let bit = 1_u8 << weekday.num_days_from_monday();
        (self.0 & bit) != 0
    }

    pub fn with_day(self, weekday: Weekday, enabled: bool) -> Self {
        let bit = 1_u8 << weekday.num_days_from_monday();
        if enabled {
            Self((self.0 | bit) & 0b0111_1111)
        } else {
            Self((self.0 & !bit) & 0b0111_1111)
        }
    }

    pub fn is_empty(self) -> bool {
        self.bits() == 0
    }
}

impl Default for WeekdayMask {
    fn default() -> Self {
        Self::EVERY_DAY
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleAction {
    /// Enforce a specific aggregate bandwidth cap (0 = unlimited).
    SpeedLimit { max_bytes_per_sec: u64 },
    /// Pause active downloads while this window is active.
    PauseQueue,
    /// Run the queue at full speed (overrides global rate cap during this window).
    #[default]
    FullSpeed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutsideHoursAction {
    /// Leave queue running state and speed limit controlled by manual user settings.
    #[default]
    UseGlobalDefault,
    /// Enforce an aggregate bandwidth cap whenever no schedule rule matches.
    LimitSpeed { max_bytes_per_sec: u64 },
    /// Automatically pause downloads whenever no schedule rule matches.
    PauseQueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub days: WeekdayMask,
    /// Minutes since 00:00 (0..1440).
    pub start_minute: u16,
    /// Minutes since 00:00 (0..1440). If `< start_minute`, wraps overnight into the next day.
    pub end_minute: u16,
    pub action: ScheduleAction,
}

impl Default for ScheduleRule {
    fn default() -> Self {
        Self {
            id: "rule-1".to_owned(),
            name: "Night full speed".to_owned(),
            enabled: true,
            days: WeekdayMask::EVERY_DAY,
            start_minute: 23 * 60,
            end_minute: 7 * 60,
            action: ScheduleAction::FullSpeed,
        }
    }
}

impl ScheduleRule {
    pub fn matches(&self, local_time: DateTime<Local>) -> bool {
        if !self.enabled || self.days.is_empty() {
            return false;
        }
        let minute = (local_time.hour() * 60 + local_time.minute()) as u16;
        let start = self.start_minute.min(MINUTES_PER_DAY - 1);
        let end = self.end_minute.min(MINUTES_PER_DAY - 1);
        let today = local_time.weekday();
        if start == end {
            return self.days.contains(today);
        }
        if start < end {
            return self.days.contains(today) && minute >= start && minute < end;
        }
        // Overnight window (e.g. 23:00..07:00): evening belongs to `today`,
        // early-morning tail belongs to yesterday's rule day.
        (self.days.contains(today) && minute >= start)
            || (self.days.contains(today.pred()) && minute < end)
    }

    pub fn normalized(mut self) -> Self {
        self.days = WeekdayMask::from_bits(self.days.bits());
        self.start_minute = self.start_minute.min(MINUTES_PER_DAY - 1);
        self.end_minute = self.end_minute.min(MINUTES_PER_DAY - 1);
        self.name = self.name.trim().to_owned();
        if self.id.trim().is_empty() {
            self.id = "rule".to_owned();
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScheduleSettings {
    pub enabled: bool,
    pub rules: Vec<ScheduleRule>,
    pub outside_hours: OutsideHoursAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScheduleDecision {
    pub active_rule_name: Option<String>,
    pub pause_queue: bool,
    /// `None` preserves global user bandwidth setting; `Some(0)` removes cap; `Some(n)` enforces `n` B/s.
    pub speed_limit_bytes_per_sec: Option<u64>,
}

impl ScheduleSettings {
    pub fn evaluate(&self, local_time: DateTime<Local>) -> ScheduleDecision {
        if !self.enabled {
            return ScheduleDecision::default();
        }

        let mut matched_names = Vec::new();
        let mut pause = false;
        let mut full_speed = false;
        let mut min_cap: Option<u64> = None;

        for rule in &self.rules {
            if !rule.matches(local_time) {
                continue;
            }
            if !rule.name.is_empty() {
                matched_names.push(rule.name.clone());
            }
            match rule.action {
                ScheduleAction::PauseQueue => {
                    pause = true;
                }
                ScheduleAction::FullSpeed
                | ScheduleAction::SpeedLimit {
                    max_bytes_per_sec: 0,
                } => {
                    full_speed = true;
                }
                ScheduleAction::SpeedLimit { max_bytes_per_sec } => {
                    min_cap = Some(match min_cap {
                        Some(existing) => existing.min(max_bytes_per_sec),
                        None => max_bytes_per_sec,
                    });
                }
            }
        }

        if !matched_names.is_empty() || pause || full_speed || min_cap.is_some() {
            let speed_limit_bytes_per_sec = if pause {
                None
            } else if let Some(cap) = min_cap {
                Some(cap)
            } else if full_speed {
                Some(0)
            } else {
                None
            };
            return ScheduleDecision {
                active_rule_name: (!matched_names.is_empty()).then(|| matched_names.join(", ")),
                pause_queue: pause,
                speed_limit_bytes_per_sec,
            };
        }

        match self.outside_hours {
            OutsideHoursAction::UseGlobalDefault => ScheduleDecision::default(),
            OutsideHoursAction::PauseQueue => ScheduleDecision {
                active_rule_name: None,
                pause_queue: true,
                speed_limit_bytes_per_sec: None,
            },
            OutsideHoursAction::LimitSpeed { max_bytes_per_sec } => ScheduleDecision {
                active_rule_name: None,
                pause_queue: false,
                speed_limit_bytes_per_sec: Some(max_bytes_per_sec),
            },
        }
    }

    pub fn normalized(mut self) -> Self {
        self.rules = self
            .rules
            .into_iter()
            .map(ScheduleRule::normalized)
            .collect();
        self
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostProcessingSettings {
    pub auto_verify_checksums: bool,
    pub auto_extract: bool,
    pub extract_into_subfolder: bool,
    pub delete_archives_after_extraction: bool,
    pub archive_passwords: Vec<String>,
    pub external_extractor_path: Option<PathBuf>,
    pub post_download_command: Option<String>,
    pub post_download_timeout_secs: u64,
}

impl Default for PostProcessingSettings {
    fn default() -> Self {
        Self {
            auto_verify_checksums: true,
            auto_extract: false,
            extract_into_subfolder: true,
            delete_archives_after_extraction: false,
            archive_passwords: Vec::new(),
            external_extractor_path: None,
            post_download_command: None,
            post_download_timeout_secs: DEFAULT_HOOK_TIMEOUT_SECS,
        }
    }
}

impl fmt::Debug for PostProcessingSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostProcessingSettings")
            .field("auto_verify_checksums", &self.auto_verify_checksums)
            .field("auto_extract", &self.auto_extract)
            .field("extract_into_subfolder", &self.extract_into_subfolder)
            .field(
                "delete_archives_after_extraction",
                &self.delete_archives_after_extraction,
            )
            .field("archive_passwords_count", &self.archive_passwords.len())
            .field("external_extractor_path", &self.external_extractor_path)
            .field(
                "has_post_download_command",
                &self.post_download_command.is_some(),
            )
            .field(
                "post_download_timeout_secs",
                &self.post_download_timeout_secs,
            )
            .finish()
    }
}

impl PostProcessingSettings {
    pub fn normalized(mut self) -> Self {
        self.archive_passwords
            .retain(|password| !password.is_empty());
        self.external_extractor_path = self.external_extractor_path.and_then(|path| {
            let trimmed = path.to_string_lossy().trim().to_owned();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        });
        self.post_download_command = self.post_download_command.and_then(|command| {
            let trimmed = command.trim().to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self.post_download_timeout_secs = self.post_download_timeout_secs.clamp(5, 3600);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WatchProcessedBehavior {
    #[default]
    MoveToProcessed,
    Delete,
    KeepTracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchFolderRule {
    pub id: String,
    pub enabled: bool,
    pub directory: PathBuf,
    pub import_torrents: bool,
    pub import_dlc: bool,
    pub import_txt: bool,
    pub category: Option<String>,
    pub destination: Option<PathBuf>,
    pub processed_behavior: WatchProcessedBehavior,
}

impl Default for WatchFolderRule {
    fn default() -> Self {
        Self {
            id: "watch-1".to_owned(),
            enabled: true,
            directory: PathBuf::new(),
            import_torrents: true,
            import_dlc: true,
            import_txt: true,
            category: None,
            destination: None,
            processed_behavior: WatchProcessedBehavior::MoveToProcessed,
        }
    }
}

impl WatchFolderRule {
    pub fn normalized(mut self) -> Self {
        if self.id.trim().is_empty() {
            self.id = "watch".to_owned();
        }
        self.category = self.category.and_then(|cat| {
            let trimmed = cat.trim().to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self.destination = self.destination.and_then(clean_optional_path);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchFolderSettings {
    pub enabled: bool,
    pub poll_interval_secs: u64,
    pub rules: Vec<WatchFolderRule>,
}

impl Default for WatchFolderSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: DEFAULT_WATCH_POLL_SECS,
            rules: Vec::new(),
        }
    }
}

impl WatchFolderSettings {
    pub fn normalized(mut self) -> Self {
        self.poll_interval_secs = self.poll_interval_secs.clamp(1, 3600);
        self.rules = self
            .rules
            .into_iter()
            .filter(|rule| !rule.directory.as_os_str().is_empty())
            .map(WatchFolderRule::normalized)
            .collect();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationSettings {
    pub schedule: ScheduleSettings,
    pub prevent_sleep_when_active: bool,
    pub completion_action: QueueCompletionAction,
    pub completion_countdown_secs: u64,
    pub post_processing: PostProcessingSettings,
    pub watch_folders: WatchFolderSettings,
}

impl Default for AutomationSettings {
    fn default() -> Self {
        Self {
            schedule: ScheduleSettings::default(),
            prevent_sleep_when_active: true,
            completion_action: QueueCompletionAction::Nothing,
            completion_countdown_secs: DEFAULT_COUNTDOWN_SECS,
            post_processing: PostProcessingSettings::default(),
            watch_folders: WatchFolderSettings::default(),
        }
    }
}

impl AutomationSettings {
    pub fn normalized(mut self) -> Self {
        self.schedule = self.schedule.normalized();
        self.completion_countdown_secs = self.completion_countdown_secs.clamp(5, 3600);
        self.post_processing = self.post_processing.normalized();
        self.watch_folders = self.watch_folders.normalized();
        self
    }
}

pub fn format_minutes(minutes: u16) -> String {
    let clamped = minutes.min(MINUTES_PER_DAY - 1);
    format!("{:02}:{:02}", clamped / 60, clamped % 60)
}

pub fn parse_hhmm(value: &str) -> Option<u16> {
    let trimmed = value.trim();
    let (hour, minute) = trimmed.split_once(':')?;
    let hour: u16 = hour.parse().ok()?;
    let minute: u16 = minute.parse().ok()?;
    if hour < 24 && minute < 60 {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

fn clean_optional_path(path: PathBuf) -> Option<PathBuf> {
    if Path::new(path.as_os_str())
        .to_string_lossy()
        .trim()
        .is_empty()
    {
        None
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn local_at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, min, 0)
            .single()
            .unwrap_or_else(Local::now)
    }

    #[test]
    fn schedule_evaluates_overnight_windows_and_previous_day_carryover() {
        // 2026-11-16 is a Monday; 2026-11-21 is Saturday; 2026-11-22 is Sunday.
        let settings = ScheduleSettings {
            enabled: true,
            outside_hours: OutsideHoursAction::LimitSpeed {
                max_bytes_per_sec: 128 * 1024,
            },
            rules: vec![ScheduleRule {
                id: "weeknights".to_owned(),
                name: "Night burst".to_owned(),
                enabled: true,
                days: WeekdayMask::WEEKDAYS,
                start_minute: 23 * 60,
                end_minute: 6 * 60,
                action: ScheduleAction::FullSpeed,
            }],
        };

        // Monday 23:30 -> active rule (unlimited = 0)
        let mon_night = settings.evaluate(local_at(2026, 11, 16, 23, 30));
        assert_eq!(mon_night.active_rule_name.as_deref(), Some("Night burst"));
        assert_eq!(mon_night.speed_limit_bytes_per_sec, Some(0));
        assert!(!mon_night.pause_queue);

        // Saturday 03:00 -> carryover from Friday night (weekday!) -> still active
        let sat_early = settings.evaluate(local_at(2026, 11, 21, 3, 0));
        assert_eq!(sat_early.speed_limit_bytes_per_sec, Some(0));

        // Sunday 03:00 -> Saturday is NOT in WEEKDAYS -> outside_hours kicks in
        let sun_early = settings.evaluate(local_at(2026, 11, 22, 3, 0));
        assert_eq!(sun_early.active_rule_name, None);
        assert_eq!(sun_early.speed_limit_bytes_per_sec, Some(128 * 1024));
    }

    #[test]
    fn schedule_overlapping_rules_prioritize_pause_and_stricter_cap() {
        let settings = ScheduleSettings {
            enabled: true,
            outside_hours: OutsideHoursAction::UseGlobalDefault,
            rules: vec![
                ScheduleRule {
                    id: "a".to_owned(),
                    name: "Work cap".to_owned(),
                    enabled: true,
                    days: WeekdayMask::EVERY_DAY,
                    start_minute: 9 * 60,
                    end_minute: 18 * 60,
                    action: ScheduleAction::SpeedLimit {
                        max_bytes_per_sec: 500_000,
                    },
                },
                ScheduleRule {
                    id: "b".to_owned(),
                    name: "Strict cap".to_owned(),
                    enabled: true,
                    days: WeekdayMask::EVERY_DAY,
                    start_minute: 12 * 60,
                    end_minute: 14 * 60,
                    action: ScheduleAction::SpeedLimit {
                        max_bytes_per_sec: 200_000,
                    },
                },
                ScheduleRule {
                    id: "c".to_owned(),
                    name: "Meeting pause".to_owned(),
                    enabled: true,
                    days: WeekdayMask::EVERY_DAY,
                    start_minute: 13 * 60,
                    end_minute: 13 * 60 + 30,
                    action: ScheduleAction::PauseQueue,
                },
            ],
        };

        let at_1230 = settings.evaluate(local_at(2026, 11, 16, 12, 30));
        assert!(!at_1230.pause_queue);
        assert_eq!(at_1230.speed_limit_bytes_per_sec, Some(200_000));

        let at_1315 = settings.evaluate(local_at(2026, 11, 16, 13, 15));
        assert!(at_1315.pause_queue);
    }

    #[test]
    fn hhmm_formatting_and_redacted_debug() {
        assert_eq!(parse_hhmm("07:45"), Some(7 * 60 + 45));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(format_minutes(23 * 60 + 5), "23:05");

        let pp = PostProcessingSettings {
            archive_passwords: vec!["top-secret-password".to_owned()],
            ..PostProcessingSettings::default()
        };
        let debug = format!("{pp:?}");
        assert!(!debug.contains("top-secret-password"));
        assert!(debug.contains("archive_passwords_count: 1"));
    }
}
