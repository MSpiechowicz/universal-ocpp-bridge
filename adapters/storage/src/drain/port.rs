use super::{Operation, Outcome};
use crate::{SqliteOperationalStore, worker::Request};
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;
use uob_application::{
    StorageFuture,
    release_drain::{DrainId, DrainObservation, ReleaseDrainPort, ReleaseJobKind},
};

impl<C, E, D, R> ReleaseDrainPort for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
    D: DeserializeOwned + Send + 'static,
    R: DeserializeOwned + Send + 'static,
{
    fn begin_drain(&self, duration: Duration) -> StorageFuture<'_, DrainId> {
        let pending = self.request(|reply| Request::Drain(Operation::Begin(duration), reply));
        Box::pin(async move {
            match pending.await? {
                Outcome::Window(id) => Ok(id),
                _ => unreachable!("typed worker reply"),
            }
        })
    }
    fn observe_drain(&self, id: DrainId) -> StorageFuture<'_, DrainObservation> {
        let pending = self.request(|reply| Request::Drain(Operation::Observe(id), reply));
        Box::pin(async move {
            match pending.await? {
                Outcome::Observation(value) => Ok(value),
                _ => unreachable!("typed worker reply"),
            }
        })
    }
    fn seal_drain(&self, observation: DrainObservation) -> StorageFuture<'_, ()> {
        done(self.request(|reply| Request::Drain(Operation::Seal(observation), reply)))
    }
    fn cancel_drain(&self, id: DrainId) -> StorageFuture<'_, ()> {
        done(self.request(|reply| Request::Drain(Operation::Cancel(id), reply)))
    }
    fn start_release_job(&self, id: String, kind: ReleaseJobKind) -> StorageFuture<'_, ()> {
        done(self.request(|reply| Request::Drain(Operation::StartJob(id, kind), reply)))
    }
    fn finish_release_job(&self, id: String) -> StorageFuture<'_, ()> {
        done(self.request(|reply| Request::Drain(Operation::FinishJob(id), reply)))
    }
}
fn done(pending: StorageFuture<'static, Outcome>) -> StorageFuture<'static, ()> {
    Box::pin(async move { pending.await.map(|_| ()) })
}
