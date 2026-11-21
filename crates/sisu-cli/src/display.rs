use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};

/// The same events drive terminal output, JSON output, and the session journal.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Started { operation: String },
    Delta { text: String },
    Finished { operation: String, elapsed_ms: u128 },
    Cancelled { operation: String },
    Failed { message: String },
    Output { kind: String, data: Value },
}

pub struct Display {
    pub json: bool,
    pub quiet: bool,
    stream_open: bool,
}

impl Display {
    pub fn new(json: bool, quiet: bool) -> Self {
        Self {
            json,
            quiet,
            stream_open: false,
        }
    }

    pub fn render(&mut self, event: &Event) -> Result<(), String> {
        if self.quiet
            && matches!(
                event,
                Event::Started { .. } | Event::Delta { .. } | Event::Finished { .. }
            )
        {
            return Ok(());
        }
        if self.json {
            let mut out = io::stdout().lock();
            serde_json::to_writer(&mut out, event).map_err(|e| e.to_string())?;
            writeln!(out).map_err(|e| e.to_string())?;
            return Ok(());
        }
        match event {
            Event::Started { operation } => eprintln!("{operation}… (Ctrl+C to cancel)"),
            Event::Delta { text } => {
                let mut out = io::stdout().lock();
                write!(out, "{text}")
                    .and_then(|_| out.flush())
                    .map_err(|e| e.to_string())?;
                self.stream_open = true;
            }
            Event::Finished {
                operation,
                elapsed_ms,
            } => eprintln!(
                "{operation} finished in {:.1}s",
                *elapsed_ms as f64 / 1000.0
            ),
            Event::Cancelled { operation } => {
                self.end_stream()?;
                eprintln!("{operation} cancelled. Session retained.");
            }
            Event::Failed { message } => {
                self.end_stream()?;
                eprintln!("Error: {message}");
            }
            Event::Output { kind, data } => {
                if kind == "response" && self.stream_open {
                    self.end_stream()?;
                } else {
                    let text = data
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| serde_json::to_string_pretty(data).unwrap_or_default());
                    writeln!(io::stdout().lock(), "{text}").map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn end_stream(&mut self) -> Result<(), String> {
        if self.stream_open {
            writeln!(io::stdout().lock()).map_err(|e| e.to_string())?;
            self.stream_open = false;
        }
        Ok(())
    }
}
