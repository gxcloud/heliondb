use std::path::PathBuf;

use rustyline::error::ReadlineError;
use rustyline::{history::FileHistory, Editor};
use tracing::warn;

use crate::client::conn::ClientConn;
use crate::client::output::print_result;

/// Interactive SQL REPL for HelionDB.
pub struct Repl {
    conn: ClientConn,
    last_query: Option<String>,
    expanded: bool,
}

impl Repl {
    /// Create a new REPL from an authenticated connection.
    pub fn new(conn: ClientConn) -> Self {
        Repl {
            conn,
            last_query: None,
            expanded: false,
        }
    }

    /// Run the REPL loop.
    pub async fn run(&mut self) {
        let history_file = Self::history_path();
        let mut rl = Editor::<(), FileHistory>::new().expect("Failed to create readline editor");
        // Ignore errors when loading history (e.g. first run, file doesn't exist)
        let _ = rl.load_history(&history_file);

        println!(
            "HelionDB interactive shell ({}:{})",
            self.conn.connection.remote_address().ip(),
            self.conn.connection.remote_address().port()
        );
        println!("Type \\? for help, \\q to quit.\n");

        loop {
            let readline = rl.readline("heliondb=> ");
            match readline {
                Ok(line) => {
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Check for backslash commands
                    if trimmed.starts_with('\\') {
                        match self.handle_backslash(&trimmed).await {
                            Action::Quit => break,
                            Action::Continue => continue,
                            Action::Error(msg) => {
                                println!("Error: {}", msg);
                            }
                        }
                        continue;
                    }

                    // Treat as SQL
                    rl.add_history_entry(&line).ok();
                    self.last_query = Some(trimmed.clone());

                    match self.conn.query(&trimmed).await {
                        Ok(result) => {
                            print_result(&result, self.expanded);
                        }
                        Err(e) => {
                            println!("Error: {}", e);
                        }
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    println!("\\q to quit");
                }
                Err(ReadlineError::Eof) => {
                    println!();
                    break;
                }
                Err(e) => {
                    println!("Readline error: {}", e);
                    break;
                }
            }
        }

        // Save history
        if let Err(e) = rl.save_history(&history_file) {
            warn!("Failed to save history: {}", e);
        }

        self.conn.close().await;
    }

    async fn handle_backslash(&mut self, cmd: &str) -> Action {
        let lower = cmd.to_lowercase();
        match lower.as_str() {
            "\\q" | "\\quit" => Action::Quit,
            "\\?" | "\\h" | "\\help" => {
                println!("HelionDB backslash commands:");
                println!("  \\q, \\quit    Quit the shell");
                println!("  \\?, \\h, \\help  Show this help");
                println!("  \\x           Toggle expanded (vertical) display");
                println!("  \\g           Re-run the last query");
                println!();
                println!("All other input is treated as SQL and sent to the server.");
                Action::Continue
            }
            "\\x" => {
                self.expanded = !self.expanded;
                println!(
                    "Expanded display is {}.",
                    if self.expanded { "on" } else { "off" }
                );
                Action::Continue
            }
            "\\g" => {
                match &self.last_query {
                    Some(sql) => match self.conn.query(sql).await {
                        Ok(result) => {
                            print_result(&result, self.expanded);
                        }
                        Err(e) => {
                            println!("Error: {}", e);
                        }
                    },
                    None => {
                        println!("No previous query.");
                    }
                }
                Action::Continue
            }
            _ => Action::Error(format!("Unknown command: {}. Try \\? for help.", cmd)),
        }
    }

    fn history_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".helionctl_history")
    }
}

enum Action {
    Quit,
    Continue,
    Error(String),
}
