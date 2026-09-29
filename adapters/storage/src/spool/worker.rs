use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, SyncSender},
};

use rusqlite::Connection;
use uob_application::{ExportSpoolError, ExportSpoolRecordAdmission, RuntimeResourceBudget};

use super::{Limits, Reply, Request, Transfer, busy, delivery, store, store_commit, unavailable};

type Active = (u64, store_commit::ActiveRecord, Arc<AtomicBool>);

struct Worker {
    connection: Connection,
    sender: Weak<SyncSender<Request>>,
    active: Option<Active>,
    next_id: u64,
    limits: Limits,
    budget: RuntimeResourceBudget,
}

pub(super) fn spawn(
    connection: Connection,
    sender: Weak<SyncSender<Request>>,
    receiver: Receiver<Request>,
    limits: Limits,
    budget: RuntimeResourceBudget,
) -> Result<(), ExportSpoolError> {
    std::thread::Builder::new()
        .name("uob-export-sqlite".into())
        .spawn(move || {
            Worker {
                connection,
                sender,
                active: None,
                next_id: 0,
                limits,
                budget,
            }
            .run(receiver);
        })
        .map_err(|_| unavailable())?;
    Ok(())
}

impl Worker {
    fn run(mut self, receiver: Receiver<Request>) {
        for request in receiver {
            if self
                .active
                .as_ref()
                .is_some_and(|(_, _, cancelled)| cancelled.load(Ordering::Acquire))
            {
                store_commit::rollback(&self.connection);
                self.active = None;
            }
            self.dispatch(request);
        }
        store_commit::rollback(&self.connection);
    }

    fn dispatch(&mut self, request: Request) {
        match request {
            Request::Wake => {}
            Request::Status(ns, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    store::status(&self.connection, &ns)
                });
            }
            Request::Observe(ns, durability, high, legacy, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    store_commit::observe(&self.connection, &ns, durability, high, legacy)
                });
            }
            Request::CommitGaps(progress, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    store_commit::commit_gaps(&self.connection, &progress, self.limits)
                });
            }
            Request::Begin(begin, reply) => self.begin(&begin, reply),
            Request::Append(id, chunk, reply) => self.append(id, &chunk, reply),
            Request::Finish(id, reply) => self.finish(id, reply),
            Request::Abort(id, reply) => self.abort(id, reply),
            Request::Pending(ns, after, limit, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    store::pending(&self.connection, &ns, after, limit, &self.budget)
                });
            }
            Request::PendingChunk(ns, descriptor, field, offset, max, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    store::pending_chunk(
                        &self.connection,
                        &ns,
                        &descriptor,
                        field,
                        offset,
                        max,
                        &self.budget,
                    )
                });
            }
            Request::Claim(ns, limit, max_bytes, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    delivery::claim(&self.connection, &ns, limit, max_bytes, &self.budget)
                });
            }
            Request::Settle(ns, report, reply) => {
                let _ = reply.send(if self.active.is_some() {
                    Err(busy())
                } else {
                    delivery::settle(&self.connection, &ns, &report)
                });
            }
        }
    }

    fn begin(
        &mut self,
        begin: &uob_application::ExportSpoolRecordBegin,
        reply: Reply<ExportSpoolRecordAdmission>,
    ) {
        if self.active.is_some() {
            let _ = reply.send(Err(busy()));
            return;
        }
        if reply.is_closed() {
            return;
        }
        match store_commit::begin(&self.connection, begin, self.limits) {
            Ok(store_commit::BeginResult::TelemetryDropped(status)) => {
                let _ = reply.send(Ok(ExportSpoolRecordAdmission::TelemetryDropped(Box::new(
                    status,
                ))));
            }
            Ok(store_commit::BeginResult::Transfer(record)) => {
                self.next_id = self.next_id.wrapping_add(1);
                let cancelled = Arc::new(AtomicBool::new(false));
                let Some(sender) = self.sender.upgrade() else {
                    store_commit::rollback(&self.connection);
                    let _ = reply.send(Err(unavailable()));
                    return;
                };
                let transfer = Transfer {
                    id: self.next_id,
                    sender,
                    cancelled: Arc::clone(&cancelled),
                    finished: false,
                };
                self.active = Some((self.next_id, record, cancelled));
                if reply
                    .send(Ok(ExportSpoolRecordAdmission::Transfer(Box::new(transfer))))
                    .is_err()
                {
                    store_commit::rollback(&self.connection);
                    self.active = None;
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn append(&mut self, id: u64, chunk: &uob_application::BudgetedRecordChunk, reply: Reply<()>) {
        let result = match self.active.as_mut() {
            Some((current, record, _)) if *current == id => {
                store_commit::append(&self.connection, record, chunk)
            }
            _ => Err(busy()),
        };
        if result.is_err()
            && self
                .active
                .as_ref()
                .is_some_and(|(current, _, _)| *current == id)
        {
            store_commit::rollback(&self.connection);
            self.active = None;
        }
        let _ = reply.send(result);
    }

    fn finish(&mut self, id: u64, reply: Reply<uob_application::ExportSpoolStatus>) {
        let result = match self.active.as_ref() {
            Some((current, record, _)) if *current == id => {
                store_commit::finish(&self.connection, record, self.limits)
            }
            _ => Err(busy()),
        };
        if self
            .active
            .as_ref()
            .is_some_and(|(current, _, _)| *current == id)
        {
            if result.is_err() {
                store_commit::rollback(&self.connection);
            }
            self.active = None;
        }
        let _ = reply.send(result);
    }

    fn abort(&mut self, id: u64, reply: Reply<()>) {
        if self
            .active
            .as_ref()
            .is_some_and(|(current, _, _)| *current == id)
        {
            store_commit::rollback(&self.connection);
            self.active = None;
        }
        let _ = reply.send(Ok(()));
    }
}
