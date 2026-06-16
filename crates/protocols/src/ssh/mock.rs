//! A fake SSH implementation for tests and the CLI demo.
//!
//! `MockSession` echoes input back and emits a small banner, so the higher
//! layers (terminal plumbing, multi-exec, UI) can be exercised without a server.

use std::collections::VecDeque;

use async_trait::async_trait;

use rrs_core::model::ConnectionProfile;

use crate::error::Result;
use crate::traits::{
    Connector, DirEntry, EntryKind, RemoteSession, ResolvedCredentials, SftpClient,
};

/// Fake connector; always "connects" successfully.
#[derive(Default)]
pub struct MockConnector;

#[async_trait]
impl Connector for MockConnector {
    async fn connect_shell(
        &self,
        profile: &ConnectionProfile,
        _creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        let banner = format!(
            "[mock] connected to '{}' ({:?})\r\n$ ",
            profile.name,
            profile.kind()
        );
        Ok(Box::new(MockSession {
            pending: VecDeque::from([banner.into_bytes()]),
        }))
    }
}

/// Fake interactive session: echoes everything written to it.
pub struct MockSession {
    pending: VecDeque<Vec<u8>>,
}

#[async_trait]
impl RemoteSession for MockSession {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        // Echo input, then a fresh prompt — enough to look alive in a demo.
        let mut echo = data.to_vec();
        echo.extend_from_slice(b"\r\n$ ");
        self.pending.push_back(echo);
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<u8>> {
        Ok(self.pending.pop_front().unwrap_or_default())
    }

    async fn resize(&mut self, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

/// In-memory SFTP for tests / demo: a tiny fixed tree.
#[derive(Default)]
pub struct MockSftp;

#[async_trait]
impl SftpClient for MockSftp {
    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        if path == "/" || path.is_empty() {
            Ok(vec![
                DirEntry {
                    name: "etc".into(),
                    kind: EntryKind::Dir,
                    size: 4096,
                    permissions: Some(0o755),
                    modified_unix: None,
                },
                DirEntry {
                    name: "home".into(),
                    kind: EntryKind::Dir,
                    size: 4096,
                    permissions: Some(0o755),
                    modified_unix: None,
                },
                DirEntry {
                    name: "readme.txt".into(),
                    kind: EntryKind::File,
                    size: 12,
                    permissions: Some(0o644),
                    modified_unix: None,
                },
            ])
        } else {
            Ok(vec![])
        }
    }

    async fn stat(&self, path: &str) -> Result<DirEntry> {
        Ok(DirEntry {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            kind: EntryKind::File,
            size: 12,
            permissions: Some(0o644),
            modified_unix: None,
        })
    }

    async fn read_file(&self, _path: &str) -> Result<Vec<u8>> {
        Ok(b"hello world\n".to_vec())
    }

    async fn write_file(&self, _path: &str, _data: &[u8]) -> Result<()> {
        Ok(())
    }
    async fn make_dir(&self, _path: &str) -> Result<()> {
        Ok(())
    }
    async fn remove_file(&self, _path: &str) -> Result<()> {
        Ok(())
    }
    async fn remove_dir(&self, _path: &str) -> Result<()> {
        Ok(())
    }
    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Ok(())
    }
    async fn set_permissions(&self, _path: &str, _mode: u32) -> Result<()> {
        Ok(())
    }
}
