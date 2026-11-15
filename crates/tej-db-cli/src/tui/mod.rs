#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::needless_pass_by_value
)]

mod render;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, style::Color, widgets::ListState};
use std::io::stdout;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
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

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
        self.status_time = Instant::now();
    }

    pub fn reload_keys(&mut self) {
        if let Ok(rx) = self.db.begin_read() {
            let range = KeyRange {
                start: Bound::Unbounded,
                end: Bound::Unbounded,
            };
            if let Ok(items) = rx.scan_range(&range) {
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
    }

    pub fn update_selected_value(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            if let Some((k, _)) = self.keys.get(idx) {
                if let Ok(rx) = self.db.begin_read() {
                    if let Ok(val) = rx.get(k) {
                        self.selected_value = val;
                        return;
                    }
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
                        "Cached: {}/{} | Hits: {} | Misses: {} | Active Readers: {}",
                        stats.cached_pages,
                        1024,
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
                        Ok(rx) => match rx.get(k) {
                            Ok(Some(val)) => {
                                let s = String::from_utf8_lossy(&val);
                                self.console_history.push((
                                    format!("Val (len {}): \"{}\"", val.len(), s),
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
                        },
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
                        tx.put(k, &v)?;
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
                        let deleted = tx.delete(k)?;
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
                    match rx.scan_range(&range) {
                        Ok(items) => {
                            self.console_history.push((
                                format!("Total entries found: {}", items.len()),
                                Color::Yellow,
                            ));
                            for item in items.iter().take(10) {
                                let k = String::from_utf8_lossy(&item.key);
                                let v = String::from_utf8_lossy(&item.value);
                                self.console_history
                                    .push((format!("  {k} -> {v}"), Color::DarkGray));
                            }
                            if items.len() > 10 {
                                self.console_history.push((
                                    format!("  ... and {} more entries", items.len() - 10),
                                    Color::DarkGray,
                                ));
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

    let res = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_loop<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| render::ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press {
                    match app.current_tab {
                        ActiveTab::Console => match key.code {
                            KeyCode::Esc => app.should_quit = true,
                            KeyCode::Tab => app.next_tab(),
                            KeyCode::BackTab => app.prev_tab(),
                            KeyCode::Enter => app.execute_console_command(),
                            KeyCode::Backspace => {
                                app.console_input.pop();
                            }
                            KeyCode::Char(c) => {
                                app.console_input.push(c);
                            }
                            _ => {}
                        },
                        _ => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                            KeyCode::Tab => app.next_tab(),
                            KeyCode::BackTab => app.prev_tab(),
                            KeyCode::Char('1') => app.current_tab = ActiveTab::Dashboard,
                            KeyCode::Char('2') => app.current_tab = ActiveTab::Explorer,
                            KeyCode::Char('3') => app.current_tab = ActiveTab::Console,
                            KeyCode::Char('r') => {
                                app.reload_keys();
                                app.set_status("Reloaded key store.");
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if !app.keys.is_empty() {
                                    let i = match app.list_state.selected() {
                                        Some(i) => {
                                            if i == 0 {
                                                app.keys.len() - 1
                                            } else {
                                                i - 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    app.list_state.select(Some(i));
                                    app.update_selected_value();
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if !app.keys.is_empty() {
                                    let i = match app.list_state.selected() {
                                        Some(i) => {
                                            if i >= app.keys.len() - 1 {
                                                0
                                            } else {
                                                i + 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    app.list_state.select(Some(i));
                                    app.update_selected_value();
                                }
                            }
                            KeyCode::Char('d') => {
                                if let Some(idx) = app.list_state.selected() {
                                    if let Some((k, _)) = app.keys.get(idx).cloned() {
                                        let del_res = (|| -> Result<(), tej_db::DbError> {
                                            let mut tx = app.db.begin_write()?;
                                            tx.delete(&k)?;
                                            tx.commit()?;
                                            Ok(())
                                        })();
                                        if del_res.is_ok() {
                                            app.reload_keys();
                                            app.set_status(format!(
                                                "Deleted key '{}'",
                                                String::from_utf8_lossy(&k)
                                            ));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        },
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
