use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};

use uob_application::{
    BudgetedRecordChunk, CommittedRecordCursor, CommittedRecordDescriptor, CommittedRecordField,
    CommittedRecordReadToken, Durability, ExportSourceCheckpoint, ExportSpoolGapCommit,
    ExportSpoolNamespace, ExportSpoolRecordAdmission, ExportSpoolRecordBegin, ExportSpoolTransfer,
    RuntimeResourceBudget, RuntimeResourceLimits, WorkClass,
};
use uob_contracts::{ExportDestination, ExportDestinationId};
use uob_storage_adapter::{ExportSpoolLimits, SqliteExportSpool};
use uuid::Uuid;

pub struct Fixture {
    pub source: PathBuf,
    pub directory: PathBuf,
    pub budget: RuntimeResourceBudget,
}

impl Fixture {
    pub fn new() -> Option<Self> {
        let source = std::env::temp_dir().join(format!("spool-source-{}.sqlite3", Uuid::new_v4()));
        let volume = PathBuf::from("/dev/shm");
        if fs::metadata(source.parent()?).ok()?.dev() == fs::metadata(&volume).ok()?.dev() {
            return None;
        }
        let directory = volume.join(format!("spool-test-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        Some(Self {
            source,
            directory,
            budget: RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap(),
        })
    }

    pub fn open(&self, bytes: u64, slots: usize) -> SqliteExportSpool {
        SqliteExportSpool::open_with_limits(
            &self.directory,
            &self.source,
            8,
            ExportSpoolLimits::new(bytes, slots).unwrap(),
            &self.budget,
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("export.sqlite3"));
        let _ = fs::remove_file(self.directory.join("export.sqlite3-journal"));
        let _ = fs::remove_dir(&self.directory);
    }
}

pub fn namespace() -> ExportSpoolNamespace {
    ExportSpoolNamespace {
        destination: ExportDestination {
            destination_id: ExportDestinationId::new("analytics").unwrap(),
            configuration_revision: 1,
        },
        provider_kind: "postgresql".into(),
        source_generation: "source-test".into(),
    }
}

pub fn checkpoint(durability: Durability, sequence: u64) -> ExportSourceCheckpoint {
    let stream = i32::from(durability != Durability::Critical);
    ExportSourceCheckpoint {
        cursor: CommittedRecordCursor::new(format!(
            "uob:record:v1:source-test:{stream}:{sequence}"
        ))
        .unwrap(),
        sequence,
    }
}

pub fn progress(durability: Durability, sequence: u64) -> ExportSpoolGapCommit {
    ExportSpoolGapCommit {
        namespace: namespace(),
        durability,
        expected: (sequence > 1).then(|| checkpoint(durability, sequence - 1)),
        next: checkpoint(durability, sequence),
        high_water: sequence,
        gaps: vec![],
        legacy_baseline_incomplete: false,
    }
}

pub fn begin(durability: Durability, sequence: u64, lengths: [u64; 3]) -> ExportSpoolRecordBegin {
    ExportSpoolRecordBegin {
        progress: progress(durability, sequence),
        descriptor: CommittedRecordDescriptor {
            token: CommittedRecordReadToken::new(
                Arc::from("source-test"),
                durability,
                sequence,
                i64::try_from(sequence).expect("fixture sequence fits SQLite integer"),
                lengths,
                0,
            ),
            durability,
            sequence,
            cursor: checkpoint(durability, sequence).cursor,
            record_id_len: lengths[0],
            committed_at_len: lengths[1],
            payload_len: lengths[2],
        },
    }
}

pub fn transfer(admission: ExportSpoolRecordAdmission) -> Box<dyn ExportSpoolTransfer> {
    match admission {
        ExportSpoolRecordAdmission::Transfer(transfer) => transfer,
        ExportSpoolRecordAdmission::TelemetryDropped(_) => panic!("record unexpectedly dropped"),
    }
}

pub async fn append_field(
    transfer: &mut Box<dyn ExportSpoolTransfer>,
    budget: &RuntimeResourceBudget,
    field: CommittedRecordField,
    bytes: &[u8],
) {
    for (index, part) in bytes.chunks(64 * 1024).enumerate() {
        let offset = index as u64 * 64 * 1024;
        let reservation = budget
            .try_reserve(WorkClass::ExporterBatch, part.len())
            .unwrap();
        let chunk = BudgetedRecordChunk::new(
            field,
            offset,
            part.to_vec(),
            bytes.len() as u64,
            reservation,
        )
        .unwrap();
        transfer.append(chunk).await.unwrap();
    }
}

pub async fn copy(
    spool: &SqliteExportSpool,
    budget: &RuntimeResourceBudget,
    durability: Durability,
    sequence: u64,
    fields: [&[u8]; 3],
) {
    use uob_application::ExportSpool;
    let lengths = fields.map(|field| field.len() as u64);
    let mut transfer = transfer(
        spool
            .begin_record(begin(durability, sequence, lengths))
            .await
            .unwrap(),
    );
    for (field, bytes) in [
        CommittedRecordField::RecordId,
        CommittedRecordField::CommittedAt,
        CommittedRecordField::Payload,
    ]
    .into_iter()
    .zip(fields)
    {
        append_field(&mut transfer, budget, field, bytes).await;
    }
    transfer.finish().await.unwrap();
}
