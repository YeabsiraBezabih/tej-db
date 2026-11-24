use crate::tui::render;
use crate::tui::{ActiveTab, App};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Terminal;
use std::time::Duration;

pub(crate) fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| render::ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press {
                    handle_key_event(app, key.code);
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_key_event(app: &mut App, code: KeyCode) {
    match app.current_tab {
        ActiveTab::Console => match code {
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
        _ => match code {
            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
            KeyCode::Tab => app.next_tab(),
            KeyCode::BackTab => app.prev_tab(),
            KeyCode::Char('1') => app.current_tab = ActiveTab::Dashboard,
            KeyCode::Char('2') => app.current_tab = ActiveTab::Explorer,
            KeyCode::Char('3') => app.current_tab = ActiveTab::Console,
            KeyCode::Char('t') => {
                app.next_table();
                app.set_status(format!("Active keyspace: [{}]", app.current_table_name()));
            }
            KeyCode::Char('r') => {
                app.reload_keys();
                app.set_status(format!("Reloaded keys for [{}].", app.current_table_name()));
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
                            if let Some(ref tbl) = app.current_table {
                                let mut t = tx.open_table_mut(tbl)?;
                                t.delete(&k)?;
                            } else {
                                tx.delete(&k)?;
                            }
                            tx.commit()?;
                            Ok(())
                        })();
                        if del_res.is_ok() {
                            app.reload_keys();
                            app.set_status(format!(
                                "Deleted key '{}' from [{}]",
                                String::from_utf8_lossy(&k),
                                app.current_table_name()
                            ));
                        }
                    }
                }
            }
            _ => {}
        },
    }
}
