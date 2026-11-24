#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::needless_pass_by_value
)]

mod events;
mod render;

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, style::Color, widgets::ListState};
use std::io::stdout;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tej_db::{Database, FileStorage, KeyRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Dashboard = 0,
    Explorer = 1,
    Console = 2,
}

pub struct App {
    pub db_path: PathBuf,
    pub db: Database<FileStorage>,
    pub current_tab: ActiveTab,
    pub should_quit: bool,
    pub status_message: String,
    pub status_time: Instant,

    // Explorer state
    pub current_table: Option<String>,
    pub keys: Vec<(Vec<u8>, usize)>,
    pub list_state: ListState,
    pub selected_value: Option<Vec<u8>>,

    // Console state
    pub console_input: String,
    pub console_history: Vec<(String, Color)>,
}

impl App {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        let db = Database::open_file(&db_path)?;
        let mut app = Self {
            db_path,
            db,
            current_tab: ActiveTab::Dashboard,
            should_quit: false,
            status_message:
                "Welcome to TejDb Interactive Shell. Press Tab to switch views, 'q' to quit.".into(),
            status_time: Instant::now(),
            current_table: None,
            keys: Vec::new(),
            list_state: ListState::default(),
            selected_value: None,
            console_input: String::new(),
            console_history: vec![
                (
                    "TejDb ACID Embedded DBMS Terminal Console ready.".into(),
                    Color::Green,
                ),
                (
                    "Type 'HELP' for available commands (PUT, GET, DEL, SCAN, STATS, SYNC).".into(),
                    Color::DarkGray,
                ),
            ],
        };
        app.reload_keys();
        Ok(app)
    }

    pub fn current_table_name(&self) -> &str {
        self.current_table.as_deref().unwrap_or("default")
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
        self.status_time = Instant::now();
    }

    pub fn next_table(&mut self) {
        let tables = self.db.list_tables().unwrap_or_default();
        if tables.is_empty() {
            self.current_table = None;
            return;
        }

        self.current_table = match &self.current_table {
            None => Some(tables[0].clone()),
            Some(cur) => {
                if let Some(pos) = tables.iter().position(|t| t == cur) {
                    if pos + 1 < tables.len() {
                        Some(tables[pos + 1].clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };

        self.list_state.select(None);
        self.reload_keys();
    }

    pub fn reload_keys(&mut self) {
        if let Ok(rx) = self.db.begin_read() {
            let range = KeyRange {
                start: Bound::Unbounded,
                end: Bound::Unbounded,
            };

            let items = if let Some(ref tbl) = self.current_table {
                rx.open_table(tbl)
                    .and_then(|t| t.scan_range(&range))
                    .unwrap_or_default()
            } else {
                rx.scan_range(&range).unwrap_or_default()
            };

            self.keys = items
                .into_iter()
                .map(|item| (item.key, item.value.len()))
                .collect();
            if !self.keys.is_empty() && self.list_state.selected().is_none() {
                self.list_state.select(Some(0));
            }
            self.update_selected_value();
        }
    }

    pub fn update_selected_value(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            if let Some((k, _)) = self.keys.get(idx) {
                if let Ok(rx) = self.db.begin_read() {
                    let val = if let Some(ref tbl) = self.current_table {
                        rx.open_table(tbl).and_then(|t| t.get(k)).ok().flatten()
                    } else {
                        rx.get(k).ok().flatten()
                    };
                    self.selected_value = val;
                    return;
                }
            }
        }
        self.selected_value = None;
    }

    pub fn next_tab(&mut self) {
        self.current_tab = match self.current_tab {
            ActiveTab::Dashboard => ActiveTab::Explorer,
            ActiveTab::Explorer => ActiveTab::Console,
            ActiveTab::Console => ActiveTab::Dashboard,
        };
    }

    pub fn prev_tab(&mut self) {
        self.current_tab = match self.current_tab {
            ActiveTab::Dashboard => ActiveTab::Console,
            ActiveTab::Explorer => ActiveTab::Dashboard,
            ActiveTab::Console => ActiveTab::Explorer,
        };
    }

    pub fn execute_console_command(&mut self) {
        let input = self.console_input.trim().to_string();
        if input.is_empty() {
            return;
        }

        self.console_history
            .push((format!("> {input}"), Color::Cyan));
        self.console_input.clear();

        let tokens: Vec<&str> = input.split_whitespace().collect();
        let cmd = tokens[0].to_uppercase();

        match cmd.as_str() {
            "HELP" => {
                self.console_history.push((
                    "Commands: PUT <k> <v> | GET <k> | DEL <k> | SCAN | STATS | SYNC | CLEAR"
                        .into(),
                    Color::Yellow,
                ));
            }
            "CLEAR" => {
                self.console_history.clear();
            }
            "SYNC" => match self.db.sync() {
                Ok(()) => self
                    .console_history
                    .push(("Storage synced to disk via fdatasync.".into(), Color::Green)),
                Err(e) => self
                    .console_history
                    .push((format!("Sync failed: {e}"), Color::Red)),
            },
            "STATS" => {
                let stats = self.db.stats();
                self.console_history.push((
                    format!(
                        "Cached: {} | Hits: {} | Misses: {} | Active Readers: {}",
                        stats.cached_pages,
                        stats.cache_hits,
                        stats.cache_misses,
                        stats.active_read_txns
                    ),
                    Color::Green,
                ));
            }
            "GET" => {
                if tokens.len() < 2 {
                    self.console_history
                        .push(("Usage: GET <key>".into(), Color::Red));
                } else {
                    let k = tokens[1].as_bytes();
                    match self.db.begin_read() {
                        Ok(rx) => {
                            let val = if let Some(ref tbl) = self.current_table {
                                rx.open_table(tbl).and_then(|t| t.get(k))
                            } else {
                                rx.get(k)
                            };
                            match val {
                                Ok(Some(v)) => {
                                    let s = String::from_utf8_lossy(&v);
                                    self.console_history.push((
                                        format!("Val (len {}): \"{}\"", v.len(), s),
                                        Color::Green,
                                    ));
                                }
                                Ok(None) => {
                                    self.console_history
                                        .push(("Key not found".into(), Color::Yellow));
                                }
                                Err(e) => self
                                    .console_history
                                    .push((format!("Error: {e}"), Color::Red)),
                            }
                        }
                        Err(e) => self
                            .console_history
                            .push((format!("Error: {e}"), Color::Red)),
                    }
                }
            }
            "PUT" => {
                if tokens.len() < 3 {
                    self.console_history
                        .push(("Usage: PUT <key> <value>".into(), Color::Red));
                } else {
                    let k = tokens[1].as_bytes();
                    let v = tokens[2..].join(" ").into_bytes();
                    let start = Instant::now();
                    let put_result = (|| -> Result<(), tej_db::DbError> {
                        let mut tx = self.db.begin_write()?;
                        if let Some(ref tbl) = self.current_table {
                            let mut t = tx.open_table_mut(tbl)?;
                            t.put(k, &v)?;
                        } else {
                            tx.put(k, &v)?;
                        }
                        tx.commit()?;
                        Ok(())
                    })();

                    match put_result {
                        Ok(()) => {
                            let ms = start.elapsed().as_secs_f64() * 1000.0;
                            self.console_history
                                .push((format!("OK (committed in {ms:.3} ms)"), Color::Green));
                            self.reload_keys();
                        }
                        Err(e) => self
                            .console_history
                            .push((format!("Put error: {e}"), Color::Red)),
                    }
                }
            }
            "DEL" | "DELETE" => {
                if tokens.len() < 2 {
                    self.console_history
                        .push(("Usage: DEL <key>".into(), Color::Red));
                } else {
                    let k = tokens[1].as_bytes();
                    let del_result = (|| -> Result<bool, tej_db::DbError> {
                        let mut tx = self.db.begin_write()?;
                        let deleted = if let Some(ref tbl) = self.current_table {
                            let mut t = tx.open_table_mut(tbl)?;
                            t.delete(k)?
                        } else {
                            tx.delete(k)?
                        };
                        tx.commit()?;
                        Ok(deleted)
                    })();

                    match del_result {
                        Ok(deleted) => {
                            if deleted {
                                self.console_history
                                    .push(("OK (deleted)".into(), Color::Green));
                            } else {
                                self.console_history
                                    .push(("Key did not exist".into(), Color::Yellow));
                            }
                            self.reload_keys();
                        }
                        Err(e) => self
                            .console_history
                            .push((format!("Delete error: {e}"), Color::Red)),
                    }
                }
            }
            "SCAN" => match self.db.begin_read() {
                Ok(rx) => {
                    let range = KeyRange {
                        start: Bound::Unbounded,
                        end: Bound::Unbounded,
                    };
                    let items_res = if let Some(ref tbl) = self.current_table {
                        rx.open_table(tbl).and_then(|t| t.scan_range(&range))
                    } else {
                        rx.scan_range(&range)
                    };
                    match items_res {
                        Ok(items) => {
                            self.console_history.push((
                                format!(
                                    "Total entries in [{}]: {}",
                                    self.current_table_name(),
                                    items.len()
                                ),
                                Color::Yellow,
                            ));
                            for item in items.iter().take(10) {
                                let k = String::from_utf8_lossy(&item.key);
                                let v = String::from_utf8_lossy(&item.value);
                                self.console_history
                                    .push((format!("  {k} -> {v}"), Color::DarkGray));
                            }
                        }
                        Err(e) => self
                            .console_history
                            .push((format!("Scan error: {e}"), Color::Red)),
                    }
                }
                Err(e) => self
                    .console_history
                    .push((format!("Error: {e}"), Color::Red)),
            },
            other => {
                self.console_history.push((
                    format!("Unknown command '{other}'. Type HELP for commands."),
                    Color::Red,
                ));
            }
        }
    }
}

pub fn run_tui(db_path: &Path) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout_handle = stdout();
    execute!(stdout_handle, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout_handle);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(db_path.to_path_buf())?;

    let res = events::run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}
