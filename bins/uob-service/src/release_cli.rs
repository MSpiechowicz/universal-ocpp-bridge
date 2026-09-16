//! Unprivileged release-manager client; it never loads bridge configuration.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::timeout,
};

pub(crate) const DEFAULT_SOCKET: &str = "/run/uob-release-manager/control.sock";
const DEADLINE: Duration = Duration::from_secs(1);
// Production preflight alone may take 300 seconds under administrator policy.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(360);
const MAX_REQUEST: usize = 1024;
// The supervisor persists at most 64 KiB. The extra 16 KiB covers the response
// envelope and event cursor metadata without accepting an unbounded peer.
const MAX_RESPONSE: usize = 80 * 1024;
const MANIFEST_LIMIT: usize = 64 * 1024;
const EVIDENCE_LIMIT: usize = 64 * 1024;

pub(crate) enum Command {
    Stage {
        bundle: PathBuf,
        socket: PathBuf,
    },
    Qualify {
        release: String,
        evidence: PathBuf,
        socket: PathBuf,
    },
    Promote {
        release: String,
        socket: PathBuf,
    },
    Rollback {
        socket: PathBuf,
    },
    Status {
        socket: PathBuf,
    },
    Events {
        after: u64,
        socket: PathBuf,
    },
}

pub(crate) async fn execute(command: Command, output: &mut impl Write) -> Result<(), Error> {
    match command {
        Command::Stage { bundle, socket } => {
            let digest = bounded_task(move || manifest_digest(&bundle)).await?;
            response_output(
                request(&socket, &Request::Stage { digest }).await?,
                Output::Mutation,
                output,
            )
        }
        Command::Qualify {
            release,
            evidence,
            socket,
        } => {
            let evidence_digest = bounded_task(move || evidence_digest(&evidence)).await?;
            response_output(
                request(
                    &socket,
                    &Request::Qualify {
                        digest: release,
                        evidence_digest,
                    },
                )
                .await?,
                Output::Mutation,
                output,
            )
        }
        Command::Promote { release, socket } => response_output(
            request(&socket, &Request::Promote { digest: release }).await?,
            Output::Mutation,
            output,
        ),
        Command::Rollback { socket } => response_output(
            request(&socket, &Request::Rollback {}).await?,
            Output::Mutation,
            output,
        ),
        Command::Status { socket } => response_output(
            request(&socket, &Request::Status {}).await?,
            Output::Status,
            output,
        ),
        Command::Events { after, socket } => response_output(
            request(&socket, &Request::Events { after }).await?,
            Output::Events,
            output,
        ),
    }
}

async fn bounded_task<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Error::Input)?
}

fn manifest_digest(bundle: &Path) -> Result<String, Error> {
    let encoded = bounded_file(&bundle.join("manifest.json"), MANIFEST_LIMIT)?;
    let manifest: ManifestDigest = serde_json::from_slice(&encoded).map_err(|_| Error::Input)?;
    if digest_name(&manifest.compatibility.artifact_digest) {
        Ok(manifest.compatibility.artifact_digest)
    } else {
        Err(Error::Input)
    }
}

fn evidence_digest(path: &Path) -> Result<String, Error> {
    use std::fmt::Write as _;
    let evidence = bounded_file(path, EVIDENCE_LIMIT)?;
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(evidence) {
        write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(digest)
}

fn bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, Error> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| Error::Input)?;
    let file = File::from(descriptor);
    let metadata = file.metadata().map_err(|_| Error::Input)?;
    if !metadata.file_type().is_file() || metadata.len() > limit as u64 {
        return Err(Error::Input);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| Error::Input)?);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Input)?;
    if bytes.len() > limit {
        return Err(Error::Input);
    }
    Ok(bytes)
}

async fn request(socket: &Path, request: &Request) -> Result<Response, Error> {
    let mut bytes = serde_json::to_vec(request).map_err(|_| Error::Protocol)?;
    if bytes.len() > MAX_REQUEST {
        return Err(Error::Protocol);
    }
    bytes.push(b'\n');
    let mut stream = timeout(DEADLINE, UnixStream::connect(socket))
        .await
        .map_err(|_| Error::Transport)?
        .map_err(|_| Error::Transport)?;
    timeout(DEADLINE, stream.write_all(&bytes))
        .await
        .map_err(|_| Error::Transport)?
        .map_err(|_| Error::Transport)?;
    let response = read_response(&mut stream).await?;
    validate_response(&response)?;
    Ok(response)
}

async fn read_response(stream: &mut UnixStream) -> Result<Response, Error> {
    let bytes = timeout(RESPONSE_DEADLINE, async {
        let mut response = Vec::with_capacity(4096);
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|_| Error::Transport)?;
            if read == 0 {
                return Err(Error::Protocol);
            }
            let Some(end) = chunk[..read].iter().position(|byte| *byte == b'\n') else {
                if response.len() + read > MAX_RESPONSE {
                    return Err(Error::Protocol);
                }
                response.extend_from_slice(&chunk[..read]);
                continue;
            };
            if end + 1 != read || response.len() + end > MAX_RESPONSE {
                return Err(Error::Protocol);
            }
            response.extend_from_slice(&chunk[..end]);
            return Ok(response);
        }
    })
    .await
    .map_err(|_| Error::Transport)??;
    serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
}

fn response_output(
    response: Response,
    output_kind: Output,
    output: &mut impl Write,
) -> Result<(), Error> {
    match output_kind {
        Output::Mutation if response.status.is_some() || response.events.is_some() => {
            Err(Error::Protocol)
        }
        Output::Status if response.events.is_some() => Err(Error::Protocol),
        Output::Status if response.code.is_status_result() && response.status.is_none() => {
            Err(Error::Protocol)
        }
        Output::Events if response.status.is_some() => Err(Error::Protocol),
        Output::Events if response.code == Code::Ok && response.events.is_none() => {
            Err(Error::Protocol)
        }
        Output::Events if response.code != Code::Ok && response.events.is_some() => {
            Err(Error::Protocol)
        }
        Output::Events if response.code == Code::Ok => match response.events {
            Some(events) => events_output(events, output),
            None => Err(Error::Protocol),
        },
        _ if response.code == Code::Ok => write_json(output, &response),
        _ => Err(Error::Policy(Box::new(response))),
    }
}

fn events_output(events: Events, output: &mut impl Write) -> Result<(), Error> {
    for record in events.records {
        write_json(output, &record)?;
    }
    write_json(
        output,
        &serde_json::json!({
            "type": "metadata",
            "cursor": events.latest_sequence,
            "truncated": events.truncated,
            "oldest_sequence": events.oldest_sequence,
            "latest_sequence": events.latest_sequence,
        }),
    )
}

fn write_json(output: &mut impl Write, value: &impl Serialize) -> Result<(), Error> {
    serde_json::to_writer(&mut *output, value)
        .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
        .map_err(|_| Error::Output)
}

fn validate_response(response: &Response) -> Result<(), Error> {
    if response.protocol != 1
        || response.manager_version.is_empty()
        || response.manager_version.len() > 64
        || !response
            .manager_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
        || response
            .status
            .as_ref()
            .is_some_and(|status| !status.valid())
        || response
            .events
            .as_ref()
            .is_some_and(|events| !events.valid())
    {
        return Err(Error::Protocol);
    }
    Ok(())
}

pub(crate) fn digest_name(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize)]
struct ManifestDigest {
    compatibility: CompatibilityDigest,
}

#[derive(Deserialize)]
struct CompatibilityDigest {
    artifact_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status {},
    Stage {
        digest: String,
    },
    Qualify {
        digest: String,
        evidence_digest: String,
    },
    Promote {
        digest: String,
    },
    Rollback {},
    Events {
        #[serde(default)]
        after: u64,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Code {
    Ok,
    Forbidden,
    InvalidRequest,
    Busy,
    ArtifactRejected,
    QualificationRequired,
    EvidenceRejected,
    PreflightRejected,
    ActivationBlocked,
    RecoveryRequired,
    StorageFailure,
}

impl Code {
    const fn is_status_result(self) -> bool {
        matches!(self, Self::Ok | Self::RecoveryRequired)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    protocol: u32,
    manager_version: String,
    code: Code,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<Events>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Status {
    rollback: Option<serde_json::Value>,
    probation: Option<serde_json::Value>,
    promotion: Option<serde_json::Value>,
    failures: Option<serde_json::Value>,
    sequence: u64,
    failed_operations: u64,
    staged_verified_digest: Option<String>,
    last_operation: Option<Record>,
    qualification: Option<serde_json::Value>,
}

impl Status {
    fn valid(&self) -> bool {
        [
            &self.rollback,
            &self.probation,
            &self.promotion,
            &self.failures,
            &self.qualification,
        ]
        .into_iter()
        .all(|value| value.as_ref().is_none_or(serde_json::Value::is_object))
            && self
                .staged_verified_digest
                .as_ref()
                .is_none_or(|digest| digest_name(digest))
            && self.last_operation.as_ref().is_none_or(Record::valid)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Events {
    records: Vec<Record>,
    oldest_sequence: u64,
    latest_sequence: u64,
    truncated: bool,
}

impl Events {
    fn valid(&self) -> bool {
        self.oldest_sequence <= self.latest_sequence.saturating_add(1)
            && self.records.len() <= 64
            && self.records.iter().all(Record::valid)
            && self.records.iter().all(|record| {
                record.sequence >= self.oldest_sequence && record.sequence <= self.latest_sequence
            })
            && self
                .records
                .windows(2)
                .all(|records| records[0].sequence < records[1].sequence)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    sequence: u64,
    uid: u32,
    request: Request,
    result: Code,
}

impl Record {
    fn valid(&self) -> bool {
        match &self.request {
            Request::Stage { digest } | Request::Promote { digest } => digest_name(digest),
            Request::Qualify {
                digest,
                evidence_digest,
            } => digest_name(digest) && digest_name(evidence_digest),
            Request::Status {} | Request::Rollback {} | Request::Events { .. } => true,
        }
    }
}

#[derive(Clone, Copy)]
enum Output {
    Mutation,
    Status,
    Events,
}

pub(crate) enum Error {
    Input,
    Transport,
    Protocol,
    Policy(Box<Response>),
    Output,
}

impl Error {
    pub(crate) fn write_safe(self, output: &mut impl Write) {
        let value = match self {
            Self::Policy(response) => serde_json::to_value(response)
                .unwrap_or_else(|_| serde_json::json!({ "error": "release_command_failed" })),
            Self::Input => serde_json::json!({ "error": "invalid_release_input" }),
            Self::Transport => serde_json::json!({ "error": "release_manager_unavailable" }),
            Self::Protocol => serde_json::json!({ "error": "release_manager_protocol_error" }),
            Self::Output => return,
        };
        let _ = serde_json::to_writer(&mut *output, &value)
            .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io));
    }

    pub(crate) const fn diagnostic(&self) -> &'static str {
        match self {
            Self::Input => "invalid release command input",
            Self::Transport => "release manager unavailable",
            Self::Protocol => "release manager protocol error",
            Self::Policy(_) => "release command rejected",
            Self::Output => "command output unavailable",
        }
    }
}
