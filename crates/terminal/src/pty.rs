//! Local-shell PTY backend (feature `pty`).
//!
//! Wraps `portable-pty`. Its reader/writer are blocking `std::io` handles, so
//! reads run on a dedicated thread and are delivered over a channel; this keeps
//! the async runtime unblocked.
//!
//! API NOTE: verify the `portable-pty` 0.8 surface (`native_pty_system`,
//! `openpty`, `spawn_command`, `try_clone_reader`, `take_writer`, `resize`).

use std::io::{Read, Write};
use std::sync::mpsc::Receiver;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::error::{Result, TerminalError};

/// A spawned local shell attached to a pseudo-terminal.
pub struct LocalPty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
}

impl LocalPty {
    /// Spawn `program` (or the default shell) in a new PTY of the given size.
    /// Returns the handle and a receiver of output chunks.
    pub fn spawn(program: Option<&str>, cols: u16, rows: u16) -> Result<(Self, Receiver<Vec<u8>>)> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| TerminalError::Pty(e.to_string()))?;

        let shell = program
            .map(|s| s.to_string())
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".to_string());
        let cmd = CommandBuilder::new(shell);

        let _child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TerminalError::Pty(e.to_string()))?;
        // Drop the slave so EOF is detected once the child exits.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| TerminalError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| TerminalError::Pty(e.to_string()))?;

        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok((Self { master: pair.master, writer }, rx))
    }

    /// Write bytes to the shell's stdin.
    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data).map_err(TerminalError::Io)?;
        self.writer.flush().map_err(TerminalError::Io)?;
        Ok(())
    }

    /// Resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| TerminalError::Pty(e.to_string()))
    }
}
