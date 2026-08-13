// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use super::FjallDurabilityMode;
use super::codec::{
    decode_rule_definition, encode_commit, encode_relation_metadata_record,
    encode_rule_definition_record, encode_tuple_record, fact_key,
};
use super::layout::{FjallKeyspaces, STATE_VERSION_KEY};
use crate::{CatalogChange, Commit, FactChangeKind};
use fjall::Database;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

pub(super) struct FjallCommitWriter {
    durability: FjallDurabilityMode,
    sender: mpsc::SyncSender<WriterMessage>,
    queued_version: AtomicU64,
    completed_version: Arc<AtomicU64>,
    write_error: Arc<Mutex<Option<String>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl FjallCommitWriter {
    pub(super) fn spawn(
        database: Database,
        keyspaces: FjallKeyspaces,
        initial_version: u64,
        durability: FjallDurabilityMode,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(1024);
        let completed_version = Arc::new(AtomicU64::new(initial_version));
        let write_error = Arc::new(Mutex::new(None));
        let writer_completed_version = completed_version.clone();
        let writer_error = write_error.clone();
        let thread = thread::Builder::new()
            .name("mica-fjall-commit-writer".to_owned())
            .spawn(move || {
                writer_loop(
                    database,
                    keyspaces,
                    receiver,
                    writer_completed_version,
                    writer_error,
                )
            })
            .map_err(|error| format!("failed to spawn fjall commit writer: {error}"))?;

        Ok(Self {
            durability,
            sender,
            queued_version: AtomicU64::new(initial_version),
            completed_version,
            write_error,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(super) fn persist_commit(&self, commit: &Commit) -> Result<(), String> {
        self.check_writer_error()?;
        match self.durability {
            FjallDurabilityMode::Relaxed => {
                self.sender
                    .send(WriterMessage::Persist {
                        commit: commit.clone(),
                        reply: None,
                    })
                    .map_err(|error| format!("fjall commit writer is stopped: {error}"))?;
                self.queued_version
                    .fetch_max(commit.version(), Ordering::AcqRel);
                Ok(())
            }
            FjallDurabilityMode::Strict => {
                let (reply_tx, reply_rx) = mpsc::channel();
                self.sender
                    .send(WriterMessage::Persist {
                        commit: commit.clone(),
                        reply: Some(reply_tx),
                    })
                    .map_err(|error| format!("fjall commit writer is stopped: {error}"))?;
                self.queued_version
                    .fetch_max(commit.version(), Ordering::AcqRel);
                reply_rx
                    .recv()
                    .map_err(|error| format!("fjall commit writer dropped reply: {error}"))?
            }
        }
    }

    pub(super) fn completed_version(&self) -> u64 {
        self.completed_version.load(Ordering::Acquire)
    }

    pub(super) fn queued_version(&self) -> u64 {
        self.queued_version.load(Ordering::Acquire)
    }

    pub(super) fn durability(&self) -> FjallDurabilityMode {
        self.durability
    }

    pub(super) fn last_write_error(&self) -> Option<String> {
        self.write_error.lock().unwrap().clone()
    }

    pub(super) fn flush(&self) -> Result<(), String> {
        self.check_writer_error()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.sender
            .send(WriterMessage::Flush { reply: reply_tx })
            .map_err(|error| format!("fjall commit writer is stopped: {error}"))?;
        reply_rx
            .recv()
            .map_err(|error| format!("fjall commit writer dropped flush reply: {error}"))?
    }

    fn check_writer_error(&self) -> Result<(), String> {
        match self.last_write_error() {
            Some(error) => Err(format!("fjall commit writer failed: {error}")),
            None => Ok(()),
        }
    }
}

impl Drop for FjallCommitWriter {
    fn drop(&mut self) {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self
            .sender
            .send(WriterMessage::Shutdown { reply: reply_tx });
        let _ = reply_rx.recv();
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

enum WriterMessage {
    Persist {
        commit: Commit,
        reply: Option<mpsc::Sender<Result<(), String>>>,
    },
    Shutdown {
        reply: mpsc::Sender<()>,
    },
    Flush {
        reply: mpsc::Sender<Result<(), String>>,
    },
}

fn writer_loop(
    database: Database,
    keyspaces: FjallKeyspaces,
    receiver: mpsc::Receiver<WriterMessage>,
    completed_version: Arc<AtomicU64>,
    write_error: Arc<Mutex<Option<String>>>,
) {
    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Persist { commit, reply } => {
                let result = write_commit(&database, &keyspaces, &commit);
                match &result {
                    Ok(()) => {
                        completed_version.fetch_max(commit.version(), Ordering::AcqRel);
                    }
                    Err(error) => {
                        *write_error.lock().unwrap() = Some(error.clone());
                    }
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            WriterMessage::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
            WriterMessage::Flush { reply } => {
                let result = match write_error.lock().unwrap().clone() {
                    Some(error) => Err(format!("fjall commit writer failed: {error}")),
                    None => Ok(()),
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn write_commit(
    database: &Database,
    keyspaces: &FjallKeyspaces,
    commit: &Commit,
) -> Result<(), String> {
    let mut batch = database.batch();
    batch.insert(
        &keyspaces.commits,
        commit.version().to_be_bytes(),
        &encode_commit(commit)?,
    );
    for change in commit.catalog_changes() {
        match change {
            CatalogChange::RelationCreated(metadata) => {
                batch.insert(
                    &keyspaces.relations,
                    metadata.id().raw().to_be_bytes(),
                    &encode_relation_metadata_record(metadata)?,
                );
            }
            CatalogChange::RuleInstalled(rule) => {
                batch.insert(
                    &keyspaces.rules,
                    rule.id().raw().to_be_bytes(),
                    &encode_rule_definition_record(rule)?,
                );
            }
            CatalogChange::RuleDisabled(rule_id) => {
                let key = rule_id.raw().to_be_bytes();
                let value = keyspaces
                    .rules
                    .get(key)
                    .map_err(|error| format!("failed to read fjall rule for disable: {error}"))?
                    .ok_or_else(|| format!("cannot disable missing persisted rule {rule_id:?}"))?;
                let mut rule = decode_rule_definition(value.as_ref())?;
                rule.deactivate();
                batch.insert(
                    &keyspaces.rules,
                    key,
                    &encode_rule_definition_record(&rule)?,
                );
            }
        }
    }
    for change in commit.changes() {
        let key = fact_key(change.relation, &change.tuple)?;
        match change.kind {
            FactChangeKind::Assert => {
                batch.insert(&keyspaces.facts, key, &encode_tuple_record(&change.tuple)?);
            }
            FactChangeKind::Retract => {
                batch.remove(&keyspaces.facts, key);
            }
        }
    }
    batch.insert(
        &keyspaces.metadata,
        STATE_VERSION_KEY,
        commit.version().to_be_bytes(),
    );
    batch.commit().map_err(|error| {
        format!(
            "failed to persist fjall commit {}: {error}",
            commit.version()
        )
    })
}
