//! Settings (JSON) + dictation history & stats (SQLite), all under %APPDATA%\VillFlowV4.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::inject::OutputMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMode {
    /// Hold the hotkey while speaking.
    #[default]
    Hold,
    /// Press once to start, again to stop.
    Toggle,
}

/// Which Gemini model command mode / polish uses (dictation is unaffected).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WritingModel {
    /// Flash-Lite: fastest answers.
    Fastest,
    /// Flash: good writing, a few seconds.
    #[default]
    Balanced,
    /// Leave Gemini on whatever model you picked (may be slow with Pro).
    Current,
}

/// "When I say X, write Y": fixed spellings for names, jargon, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DictEntry {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: String,
    pub hotkey: String,
    /// Command mode: edit the selection / generate at the cursor. "" = off.
    pub command_hotkey: String,
    pub trigger: TriggerMode,
    pub output: OutputMode,
    pub restore_clipboard: bool,
    pub add_trailing_space: bool,
    pub launch_at_login: bool,
    pub save_history: bool,
    /// Typing speed used for the "time saved" estimate.
    pub typing_wpm: u32,
    /// Unload the provider page after this many idle minutes (0 = keep it ready).
    pub free_memory_after_mins: u32,
    /// Have the AI clean up each dictation (fillers, punctuation). Adds a few seconds.
    pub polish: bool,
    /// Which provider does the writing (command mode, polish): "same" as the
    /// dictation provider, or a provider id ("chatgpt", "gemini").
    pub writer: String,
    pub writing_model: WritingModel,
    pub dictionary: Vec<DictEntry>,
    pub onboarded: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: "gemini".into(),
            hotkey: "Ctrl+Shift+Space".into(),
            command_hotkey: "Ctrl+Shift+X".into(),
            trigger: TriggerMode::Hold,
            output: OutputMode::Paste,
            restore_clipboard: true,
            add_trailing_space: true,
            launch_at_login: false,
            save_history: true,
            typing_wpm: 40,
            free_memory_after_mins: 0,
            polish: false,
            writer: "same".into(),
            writing_model: WritingModel::Balanced,
            dictionary: Vec::new(),
            onboarded: false,
        }
    }
}

/// The providers VillFlow supports (Gemini first: it's the default).
pub const PROVIDERS: [&str; 2] = ["gemini", "chatgpt"];

impl Settings {
    /// Fix values that can't work: a provider that's no longer supported (Grok,
    /// in versions before 0.7) falls back to Gemini, an unknown writer to "same".
    /// Returns true if anything changed.
    pub fn normalize(&mut self) -> bool {
        let mut changed = false;
        if !PROVIDERS.contains(&self.provider.as_str()) {
            self.provider = "gemini".into();
            changed = true;
        }
        if self.writer != "same" && !PROVIDERS.contains(&self.writer.as_str()) {
            self.writer = "same".into();
            changed = true;
        }
        if !(5..=200).contains(&self.typing_wpm) {
            self.typing_wpm = self.typing_wpm.clamp(5, 200);
            changed = true;
        }
        changed
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: i64,
    pub ts: i64,
    pub text: String,
    pub words: i64,
    pub duration_ms: i64,
    pub provider: String,
    pub app: String,
    /// "dictation", "edit" or "generate".
    pub kind: String,
    /// For edit/generate: the spoken instruction.
    pub note: String,
}

/// One finished dictation or command, to record.
pub struct NewEntry<'a> {
    pub text: &'a str,
    pub duration_ms: i64,
    pub provider: &'a str,
    pub app: &'a str,
    pub kind: &'a str,
    pub note: &'a str,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub dictations: i64,
    pub words: i64,
    pub speaking_ms: i64,
    pub words_today: i64,
    pub dictations_today: i64,
    /// Typing time minus speaking time, in seconds.
    pub saved_secs: i64,
    pub avg_wpm: i64,
    pub streak_days: i64,
    pub top_apps: Vec<(String, i64)>,
    /// Words per day for the last 14 days, oldest first (the last one is today).
    pub daily: Vec<i64>,
}

pub struct Store {
    dir: PathBuf,
    db: Mutex<Connection>,
    settings: Mutex<Settings>,
}

pub fn data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("VillFlowV4")
}

pub fn word_count(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

impl Store {
    pub fn open() -> anyhow::Result<Self> {
        Self::open_at(data_dir())
    }

    pub fn open_at(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let db = Connection::open(dir.join("history.sqlite3"))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS dictations (
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                text TEXT NOT NULL,
                words INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                provider TEXT NOT NULL,
                app TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS dictations_ts ON dictations(ts);
             -- Running totals survive history deletion (stats keep counting).
             CREATE TABLE IF NOT EXISTS totals (
                k TEXT PRIMARY KEY,
                v INTEGER NOT NULL
             );",
        )?;
        // Columns added after the first release.
        for (col, ddl) in [
            (
                "kind",
                "ALTER TABLE dictations ADD COLUMN kind TEXT NOT NULL DEFAULT 'dictation'",
            ),
            (
                "note",
                "ALTER TABLE dictations ADD COLUMN note TEXT NOT NULL DEFAULT ''",
            ),
        ] {
            let exists: bool = db
                .prepare("SELECT 1 FROM pragma_table_info('dictations') WHERE name = ?1")?
                .exists(params![col])?;
            if !exists {
                db.execute(ddl, [])?;
            }
        }
        let settings_path = dir.join("settings.json");
        let mut settings = match std::fs::read_to_string(&settings_path) {
            Err(_) => Settings::default(),
            // A byte-order mark (Notepad, PowerShell) isn't valid JSON; skip it.
            Ok(raw) => {
                serde_json::from_str(raw.trim_start_matches('\u{feff}')).unwrap_or_else(|e| {
                    // Keep the broken file for the user instead of silently overwriting it.
                    log::warn!("settings.json is invalid ({e}); using defaults");
                    let _ = std::fs::rename(&settings_path, dir.join("settings.corrupt.json"));
                    Settings::default()
                })
            }
        };
        let migrated = settings.normalize();
        let store = Self {
            dir,
            db: Mutex::new(db),
            settings: Mutex::new(settings.clone()),
        };
        if migrated {
            store.save_settings(settings)?;
        }
        Ok(store)
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn save_settings(&self, s: Settings) -> anyhow::Result<()> {
        let tmp = self.dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&s)?)?;
        std::fs::rename(&tmp, self.dir.join("settings.json"))?;
        *self.settings.lock().unwrap() = s;
        Ok(())
    }

    /// Record a finished dictation. Totals always update; the text is kept only
    /// when history is enabled.
    pub fn record(&self, e: NewEntry, keep_text: bool) -> anyhow::Result<Option<Entry>> {
        let NewEntry {
            text,
            duration_ms,
            provider,
            app,
            kind,
            note,
        } = e;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let ts = now_ms();
        let words = word_count(text);
        let day = local_day(ts);
        // Running totals (and per-day / per-app ones) live apart from the history
        // rows, so stats survive deleting history or turning it off.
        let mut counters = vec![
            ("dictations".to_string(), 1),
            ("words".to_string(), words),
            ("speaking_ms".to_string(), duration_ms),
            (format!("day:{day}"), words),
            (format!("dday:{day}"), 1),
        ];
        if !app.is_empty() {
            counters.push((format!("app:{app}"), words));
        }
        for (k, v) in counters {
            tx.execute(
                "INSERT INTO totals(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = v + ?2",
                params![k, v],
            )?;
        }
        let id = if keep_text {
            tx.execute(
                "INSERT INTO dictations(ts, text, words, duration_ms, provider, app, kind, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![ts, text, words, duration_ms, provider, app, kind, note],
            )?;
            Some(tx.last_insert_rowid())
        } else {
            None
        };
        tx.commit()?;
        let Some(id) = id else {
            return Ok(None);
        };
        Ok(Some(Entry {
            id,
            ts,
            text: text.to_string(),
            words,
            duration_ms,
            provider: provider.to_string(),
            app: app.to_string(),
            kind: kind.to_string(),
            note: note.to_string(),
        }))
    }

    #[cfg(test)]
    pub fn history(&self, query: &str, limit: i64, offset: i64) -> anyhow::Result<Vec<Entry>> {
        self.history_of("", query, limit, offset)
    }

    /// History filtered by kind: "" = everything, "dictation" = plain dictations,
    /// "ai" = command-mode entries (edit / ask / continue / write).
    pub fn history_of(
        &self,
        kind: &str,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Entry>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT id, ts, text, words, duration_ms, provider, app, kind, note FROM dictations
             WHERE (?1 = '' OR text LIKE '%' || ?1 || '%' ESCAPE '\\'
                            OR note LIKE '%' || ?1 || '%' ESCAPE '\\')
               AND (?4 = '' OR (?4 = 'dictation' AND kind = 'dictation')
                            OR (?4 = 'ai' AND kind <> 'dictation'))
             ORDER BY ts DESC LIMIT ?2 OFFSET ?3",
        )?;
        // Search literally: "50%" or "snake_case" shouldn't act as wildcards.
        let query = query
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let rows = stmt.query_map(params![query, limit, offset, kind], |r| {
            Ok(Entry {
                id: r.get(0)?,
                ts: r.get(1)?,
                text: r.get(2)?,
                words: r.get(3)?,
                duration_ms: r.get(4)?,
                provider: r.get(5)?,
                app: r.get(6)?,
                kind: r.get(7)?,
                note: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn delete(&self, id: i64) -> anyhow::Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("DELETE FROM dictations WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear_history(&self) -> anyhow::Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("DELETE FROM dictations", [])?;
        Ok(())
    }

    pub fn stats(&self) -> anyhow::Result<Stats> {
        let typing_wpm = self.settings().typing_wpm.max(1) as i64;
        let db = self.db.lock().unwrap();
        let total = |k: &str| -> i64 {
            db.query_row("SELECT v FROM totals WHERE k = ?1", params![k], |r| {
                r.get(0)
            })
            .unwrap_or(0)
        };
        let dictations = total("dictations");
        let words = total("words");
        let speaking_ms = total("speaking_ms");
        let today = local_day(now_ms());
        let words_today = total(&format!("day:{today}"));
        let dictations_today = total(&format!("dday:{today}"));

        let saved_secs = (words * 60 / typing_wpm - speaking_ms / 1000).max(0);
        let avg_wpm = if speaking_ms > 0 {
            words * 60_000 / speaking_ms
        } else {
            0
        };

        let per_day: Vec<(i64, i64)> = db
            .prepare("SELECT k, v FROM totals WHERE k LIKE 'day:%'")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|row| {
                let (k, v) = row.ok()?;
                Some((k.strip_prefix("day:")?.parse().ok()?, v))
            })
            .collect();
        let daily = (0..14)
            .rev()
            .map(|ago| {
                per_day
                    .iter()
                    .find(|(d, _)| *d == today - ago)
                    .map_or(0, |(_, v)| *v)
            })
            .collect();

        // Consecutive days (ending today or yesterday) with at least one dictation.
        let mut days: Vec<i64> = per_day.iter().map(|(d, _)| *d).collect();
        days.sort_unstable_by(|a, b| b.cmp(a));
        let mut streak = 0;
        let mut expect = if days.first() == Some(&today) {
            today
        } else {
            today - 1
        };
        for d in days {
            if d == expect {
                streak += 1;
                expect -= 1;
            } else if d < expect {
                break;
            }
        }

        let mut top_apps: Vec<(String, i64)> = db
            .prepare("SELECT k, v FROM totals WHERE k LIKE 'app:%' ORDER BY v DESC LIMIT 5")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (k, _) in &mut top_apps {
            *k = k.trim_start_matches("app:").to_string();
        }

        Ok(Stats {
            dictations,
            words,
            speaking_ms,
            words_today,
            dictations_today,
            saved_secs,
            avg_wpm,
            streak_days: streak,
            top_apps,
            daily,
        })
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Local UTC offset in ms (Windows API; no chrono dependency needed).
fn utc_offset_ms() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz = TIME_ZONE_INFORMATION::default();
    let kind = unsafe { GetTimeZoneInformation(&mut tz) };
    // 2 = TIME_ZONE_ID_DAYLIGHT
    let bias = tz.Bias
        + if kind == 2 {
            tz.DaylightBias
        } else {
            tz.StandardBias
        };
    -(bias as i64) * 60_000
}

/// Days since epoch in local time.
fn local_day(ts_ms: i64) -> i64 {
    (ts_ms + utc_offset_ms()).div_euclid(86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("villflow-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::open_at(dir).unwrap()
    }

    #[test]
    fn records_history_and_totals() {
        let s = temp_store("record");
        s.record(
            NewEntry {
                text: "hello there world",
                duration_ms: 3000,
                provider: "chatgpt",
                app: "notepad",
                kind: "dictation",
                note: "",
            },
            true,
        )
        .unwrap();
        s.record(
            NewEntry {
                text: "second one",
                duration_ms: 1000,
                provider: "gemini",
                app: "notepad",
                kind: "dictation",
                note: "",
            },
            false,
        )
        .unwrap();
        let h = s.history("", 10, 0).unwrap();
        assert_eq!(h.len(), 1, "history off → text not stored");
        let st = s.stats().unwrap();
        assert_eq!(st.dictations, 2);
        assert_eq!(st.words, 5);
        assert_eq!(st.dictations_today, 2, "counted even with history off");
        assert_eq!(st.words_today, 5);
        assert_eq!(st.streak_days, 1);
        assert_eq!(st.daily.len(), 14);
        assert_eq!(st.daily[13], 5, "today is last");
        assert_eq!(st.daily[..13].iter().sum::<i64>(), 0);
        assert_eq!(st.top_apps, vec![("notepad".to_string(), 5)]);
        // 5 words at 40 wpm = 7.5 s typing vs 4 s speaking.
        assert_eq!(st.saved_secs, 3);
    }

    #[test]
    fn stats_survive_history_clear() {
        let s = temp_store("clear");
        s.record(
            NewEntry {
                text: "one two three",
                duration_ms: 1000,
                provider: "chatgpt",
                app: "",
                kind: "dictation",
                note: "",
            },
            true,
        )
        .unwrap();
        s.clear_history().unwrap();
        assert!(s.history("", 10, 0).unwrap().is_empty());
        assert_eq!(s.stats().unwrap().words, 3);
    }

    #[test]
    fn search_is_literal() {
        let s = temp_store("search");
        s.record(
            NewEntry {
                text: "grew 50% this year",
                duration_ms: 1000,
                provider: "chatgpt",
                app: "",
                kind: "dictation",
                note: "",
            },
            true,
        )
        .unwrap();
        s.record(
            NewEntry {
                text: "grew 500 units",
                duration_ms: 1000,
                provider: "chatgpt",
                app: "",
                kind: "dictation",
                note: "",
            },
            true,
        )
        .unwrap();
        s.record(
            NewEntry {
                text: "use snake_case names",
                duration_ms: 1000,
                provider: "chatgpt",
                app: "",
                kind: "dictation",
                note: "",
            },
            true,
        )
        .unwrap();
        s.record(
            NewEntry {
                text: "use snakeXcase names",
                duration_ms: 1000,
                provider: "chatgpt",
                app: "",
                kind: "edit",
                note: "",
            },
            true,
        )
        .unwrap();
        assert_eq!(s.history("50%", 10, 0).unwrap().len(), 1);
        assert_eq!(s.history_of("ai", "", 10, 0).unwrap().len(), 1);
        assert_eq!(s.history_of("dictation", "", 10, 0).unwrap().len(), 3);
        assert_eq!(s.history("snake_case", 10, 0).unwrap().len(), 1);
        assert_eq!(
            s.history("GREW", 10, 0).unwrap().len(),
            2,
            "case-insensitive"
        );
    }

    #[test]
    fn corrupt_settings_fall_back_to_defaults() {
        let dir =
            std::env::temp_dir().join(format!("villflow-test-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), "{ not json").unwrap();
        let s = Store::open_at(dir.clone()).unwrap();
        assert_eq!(s.settings().hotkey, Settings::default().hotkey);
        assert!(
            dir.join("settings.corrupt.json").exists(),
            "bad file kept for the user"
        );
    }

    #[test]
    fn settings_with_a_byte_order_mark_load() {
        let dir = std::env::temp_dir().join(format!("villflow-test-bom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            "\u{feff}{\"hotkey\":\"Ctrl+Alt+B\"}",
        )
        .unwrap();
        let s = Store::open_at(dir.clone()).unwrap();
        assert_eq!(s.settings().hotkey, "Ctrl+Alt+B");
        assert!(!dir.join("settings.corrupt.json").exists());
    }

    #[test]
    fn grok_settings_migrate_to_gemini() {
        let dir = std::env::temp_dir().join(format!("villflow-test-grok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"provider":"grok","writer":"grok","hotkey":"Ctrl+Alt+D","typing_wpm":0}"#,
        )
        .unwrap();
        let s = Store::open_at(dir.clone()).unwrap();
        let st = s.settings();
        assert_eq!(st.provider, "gemini");
        assert_eq!(st.writer, "same");
        assert_eq!(st.typing_wpm, 5);
        assert_eq!(st.hotkey, "Ctrl+Alt+D", "other settings kept");
        let saved = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(saved.contains("\"gemini\""), "migration written to disk");
        // A supported writer is kept.
        let mut ok = Settings {
            writer: "chatgpt".into(),
            ..Settings::default()
        };
        assert!(!ok.normalize());
        assert_eq!(ok.writer, "chatgpt");
    }
}
