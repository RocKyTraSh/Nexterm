//! Local-shell transport (feature `local-pty`).
//!
//! Adapts the terminal crate's blocking [`LocalPty`] backend to the async
//! [`RemoteSession`] trait, so a local shell is just another transport behind
//! `AppCore` — selected for [`ProtocolSettings::LocalShell`] profiles, no
//! network involved.
//!
//! Blocking work (opening the PTY, forking the child, and the blocking
//! `recv` from the reader thread) runs on `spawn_blocking`, never on the async
//! worker — see invariant 5 in `CLAUDE.md`.

use std::sync::mpsc::Receiver;

use async_trait::async_trait;

use rrs_core::model::{ConnectionProfile, ProtocolSettings};
use rrs_terminal::pty::LocalPty;

use crate::error::{ProtocolError, Result};
use crate::traits::{Connector, RemoteSession, ResolvedCredentials};

/// Initial PTY geometry (the conventional 80x24); the frontend resizes on attach.
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Opens local-shell sessions backed by a pseudo-terminal.
#[derive(Default)]
pub struct LocalShellConnector;

#[async_trait]
impl Connector for LocalShellConnector {
    async fn connect_shell(
        &self,
        profile: &ConnectionProfile,
        _creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        let program = match &profile.settings {
            ProtocolSettings::LocalShell { program } => program.clone(),
            other => {
                return Err(ProtocolError::Connect(format!(
                    "LocalShellConnector cannot open a {:?} profile",
                    other.kind()
                )))
            }
        };

        // openpty + spawn_command fork a child and touch the filesystem; keep
        // them off the async worker.
        let (pty, rx) = tokio::task::spawn_blocking(move || {
            LocalPty::spawn(program.as_deref(), DEFAULT_COLS, DEFAULT_ROWS)
        })
        .await
        .map_err(|e| ProtocolError::Connect(format!("pty spawn task failed: {e}")))?
        .map_err(|e| ProtocolError::Connect(e.to_string()))?;

        Ok(Box::new(LocalPtySession { pty, rx: Some(rx) }))
    }
}

/// A local shell attached to a PTY, exposed as a [`RemoteSession`].
pub struct LocalPtySession {
    pty: LocalPty,
    /// Output channel from the PTY reader thread. Held in an `Option` so it can
    /// be moved into `spawn_blocking` for a blocking `recv` and put back.
    rx: Option<Receiver<Vec<u8>>>,
}

#[async_trait]
impl RemoteSession for LocalPtySession {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.pty
            .write_input(data)
            .map_err(|e| ProtocolError::Channel(e.to_string()))
    }

    async fn read(&mut self) -> Result<Vec<u8>> {
        let rx = match self.rx.take() {
            Some(rx) => rx,
            // Already closed: report clean EOF rather than erroring.
            None => return Ok(Vec::new()),
        };
        // The reader thread delivers over a blocking std channel; receive off
        // the async pool.
        let (rx, chunk) = tokio::task::spawn_blocking(move || {
            // `recv` errs only once the reader thread is gone → clean EOF.
            let chunk = rx.recv().ok();
            (rx, chunk)
        })
        .await
        .map_err(|e| ProtocolError::Channel(format!("pty read task failed: {e}")))?;
        match chunk {
            Some(bytes) => {
                self.rx = Some(rx);
                Ok(bytes)
            }
            // EOF: drop the receiver and signal end-of-stream with an empty Vec.
            None => Ok(Vec::new()),
        }
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.pty
            .resize(cols, rows)
            .map_err(|e| ProtocolError::Channel(e.to_string()))
    }

    async fn close(&mut self) -> Result<()> {
        // Dropping the receiver stops the reader thread; the PTY master (and so
        // the child's controlling terminal) is released when `self` is dropped.
        self.rx = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrs_core::model::ConnectionProfile;

    #[tokio::test]
    async fn local_shell_runs_a_command_and_reaches_eof() {
        let connector = LocalShellConnector;
        let profile = ConnectionProfile::new_local_shell("test", Some("/bin/sh".into()));
        let creds = ResolvedCredentials::default();
        let mut session = connector
            .connect_shell(&profile, &creds)
            .await
            .expect("connect local shell");

        session
            .write(b"echo rrs_marker_42\nexit\n")
            .await
            .expect("write command");

        let mut out = Vec::new();
        loop {
            let chunk = session.read().await.expect("read chunk");
            if chunk.is_empty() {
                break; // clean EOF once the shell exits
            }
            out.extend_from_slice(&chunk);
        }

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("rrs_marker_42"), "shell output was: {text:?}");
    }
}
