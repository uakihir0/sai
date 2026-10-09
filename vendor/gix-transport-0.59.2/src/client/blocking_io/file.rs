use std::{
    any::Any,
    borrow::Cow,
    error::Error,
    ffi::OsString,
    io::Read,
    process::{self, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use bstr::{BStr, BString, ByteSlice};

use crate::{
    Protocol, Service,
    client::{
        self, MessageKind, WriteMode,
        blocking_io::{FetchTransferLimit, RequestWriter, SetServiceResponse, ssh},
        git::blocking_io::Connection,
    },
};

// from https://github.com/git/git/blob/20de7e7e4f4e9ae52e6cc7cfaa6469f186ddb0fa/environment.c#L115:L115
const ENV_VARS_TO_REMOVE: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_COUNT",
];

/// A utility to spawn a helper process to actually transmit data, possibly over `ssh`.
///
/// It can only be instantiated using the local [`connect()`] or [ssh connect][super::ssh::connect()].
pub struct SpawnProcessOnDemand {
    desired_version: Protocol,
    url: gix_url::Url,
    path: BString,
    ssh_cmd: Option<(OsString, ssh::ProgramKind)>,
    /// The environment variables to set in the invoked command.
    envs: Vec<(&'static str, String)>,
    ssh_disallow_shell: bool,
    connection: Option<Connection<Box<dyn std::io::Read + Send>, process::ChildStdin>>,
    child: Option<process::Child>,
    subprocess_read_timeout: Option<Duration>,
    fetch_transfer_limit: Option<FetchTransferLimit>,
    stdout_reader_thread: Option<JoinHandle<()>>,
    stderr_reader_thread: Option<JoinHandle<()>>,
    trace: bool,
}

impl SpawnProcessOnDemand {
    pub(crate) fn new_ssh(
        url: gix_url::Url,
        program: impl Into<OsString>,
        path: BString,
        ssh_kind: ssh::ProgramKind,
        ssh_disallow_shell: bool,
        version: Protocol,
        trace: bool,
    ) -> SpawnProcessOnDemand {
        SpawnProcessOnDemand {
            url,
            path,
            ssh_cmd: Some((program.into(), ssh_kind)),
            envs: Default::default(),
            ssh_disallow_shell,
            child: None,
            connection: None,
            subprocess_read_timeout: None,
            fetch_transfer_limit: None,
            stdout_reader_thread: None,
            stderr_reader_thread: None,
            desired_version: version,
            trace,
        }
    }

    fn new_local(path: BString, version: Protocol, trace: bool) -> SpawnProcessOnDemand {
        SpawnProcessOnDemand {
            url: gix_url::Url::from_parts(
                gix_url::Scheme::File,
                None,
                None,
                None,
                None,
                path.clone(),
                true,
            )
            .expect("valid url"),
            path,
            ssh_cmd: None,
            envs: if version != Protocol::V1 {
                vec![("GIT_PROTOCOL", format!("version={}", version as usize))]
            } else {
                Default::default()
            },
            ssh_disallow_shell: false,
            child: None,
            connection: None,
            subprocess_read_timeout: None,
            fetch_transfer_limit: None,
            stdout_reader_thread: None,
            stderr_reader_thread: None,
            desired_version: version,
            trace,
        }
    }

    fn prepare_command(
        &self,
        service: Service,
    ) -> Result<(gix_command::Prepare, Option<ssh::ProgramKind>, OsString), client::Error> {
        let (mut cmd, ssh_kind, cmd_name) = match &self.ssh_cmd {
            Some((command, kind)) => (
                kind.prepare_invocation(
                    command,
                    &self.url,
                    self.desired_version,
                    self.ssh_disallow_shell,
                )
                .map_err(client::Error::SshInvocation)?
                .stderr(Stdio::piped()),
                Some(*kind),
                command.to_owned(),
            ),
            None => (
                gix_command::prepare(service.as_str()).stderr(Stdio::null()),
                None,
                OsString::from(service.as_str()),
            ),
        };
        if self.path.trim().first() == Some(&b'-') {
            return Err(client::Error::AmbiguousPath {
                path: self.path.clone(),
            });
        }
        let repo_path = if self.ssh_cmd.is_some() {
            cmd.args.push(service.as_str().into());
            gix_quote::single(self.path.as_ref())
                .to_os_str_lossy()
                .into_owned()
        } else {
            self.path.to_os_str_lossy().into_owned()
        };
        cmd.args.push(repo_path);
        Ok((cmd, ssh_kind, cmd_name))
    }

    /// Prepare an invocation for when the program of `service`, e.g. `git-upload-pack`, isn't present in `PATH`,
    /// which can happen on Windows in particular, where `git` may be installed without its subcommands
    /// being directly available (#2313).
    ///
    /// Prefer the same program from git's own `--exec-path`, which is where `git` itself finds it, and
    /// otherwise let the `git` itself run it as subcommand.
    /// This is only used for local repositories - remote shells keep the standard invocation.
    fn prepare_fallback_command(&self, service: Service) -> (gix_command::Prepare, OsString) {
        let (mut cmd, cmd_name) = match gix_path::env::core_dir_program(service.as_str()) {
            Some(program) => {
                let cmd_name = program.clone().into_os_string();
                (
                    gix_command::prepare(program).stderr(Stdio::null()),
                    cmd_name,
                )
            }
            None => {
                let subcommand = service.as_git_subcommand();
                let git = gix_path::env::exe_invocation();
                let cmd = gix_command::prepare(git)
                    .stderr(Stdio::null())
                    .arg(subcommand);

                let mut cmd_name: OsString = git.into();
                cmd_name.push(" ");
                cmd_name.push(subcommand);
                (cmd, cmd_name)
            }
        };
        cmd.args.push(self.path.to_os_str_lossy().into_owned());
        (cmd, cmd_name)
    }

    fn cleanup_subprocess(&mut self) {
        if let Some(mut child) = self.child.take() {
            // The child process (e.g. `ssh`) may still be running at this point, so kill it before joining/waiting.
            // In the happy-path case, it should have already exited gracefully, but in error cases or if the user
            // interrupted the operation, it will likely still be running.
            child.kill().ok();
            child.wait().ok();
        }
        self.connection.take();
        for reader in [
            self.stdout_reader_thread.take(),
            self.stderr_reader_thread.take(),
        ]
        .into_iter()
        .flatten()
        {
            reader.join().ok();
        }
    }
}

impl client::TransportWithoutIO for SpawnProcessOnDemand {
    fn set_identity(&mut self, identity: gix_sec::identity::Account) -> Result<(), client::Error> {
        if self.url.scheme == gix_url::Scheme::Ssh {
            self.url
                .set_user((!identity.username.is_empty()).then_some(identity.username));
            Ok(())
        } else {
            Err(client::Error::AuthenticationUnsupported)
        }
    }

    fn to_url(&self) -> Cow<'_, BStr> {
        Cow::Owned(self.url.to_bstring())
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        true
    }

    fn configure(
        &mut self,
        config: &dyn Any,
    ) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
        self.subprocess_read_timeout = config
            .downcast_ref::<super::SubprocessReadTimeout>()
            .map(super::SubprocessReadTimeout::duration);
        self.fetch_transfer_limit = config
            .downcast_ref::<super::SubprocessReadTimeout>()
            .and_then(|options| options.transfer_limit().cloned());
        Ok(())
    }
}

impl Drop for SpawnProcessOnDemand {
    fn drop(&mut self) {
        self.cleanup_subprocess();
    }
}

enum StdoutMessage {
    Data(Vec<u8>),
    Error(std::io::Error),
    Eof,
}

struct ReadStdoutWithDeadline {
    recv: Receiver<StdoutMessage>,
    ssh_errors: Option<Receiver<SshStderrMessage>>,
    buffered: Vec<u8>,
    buffered_offset: usize,
    deadline: Option<Instant>,
}

enum SshStderrMessage {
    Error(std::io::Error),
    Done,
}

impl Read for ReadStdoutWithDeadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.check_deadline()?;
        self.check_ssh_error()?;

        if self.buffered_offset < self.buffered.len() {
            let available = &self.buffered[self.buffered_offset..];
            let copied = buf.len().min(available.len());
            buf[..copied].copy_from_slice(&available[..copied]);
            self.buffered_offset += copied;
            return Ok(copied);
        }

        let message = self.receive_message()?;
        match message {
            StdoutMessage::Data(buffered) => {
                self.buffered = buffered;
                self.buffered_offset = 0;
                self.read(buf)
            }
            StdoutMessage::Error(error) => Err(error),
            StdoutMessage::Eof => {
                if self.ssh_errors.is_some() {
                    loop {
                        let wait = self.deadline.map_or(Duration::from_millis(10), |deadline| {
                            deadline.saturating_duration_since(Instant::now())
                        });
                        let result = self
                            .ssh_errors
                            .as_ref()
                            .expect("checked above")
                            .recv_timeout(wait);
                        match result {
                            Ok(SshStderrMessage::Error(error)) => {
                                self.check_deadline()?;
                                return Err(error);
                            }
                            Ok(SshStderrMessage::Done) => {
                                self.check_deadline()?;
                                self.ssh_errors = None;
                                break;
                            }
                            Err(RecvTimeoutError::Timeout) => {
                                self.check_deadline()?;
                                if self.deadline.is_none() {
                                    break;
                                }
                            }
                            Err(RecvTimeoutError::Disconnected) => {
                                self.ssh_errors = None;
                                break;
                            }
                        }
                    }
                }
                self.check_deadline()?;
                Ok(0)
            }
        }
    }
}

impl ReadStdoutWithDeadline {
    fn check_deadline(&self) -> std::io::Result<()> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Git transport operation timed out",
            ))
        } else {
            Ok(())
        }
    }

    fn check_ssh_error(&mut self) -> std::io::Result<()> {
        let result = self.ssh_errors.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(SshStderrMessage::Error(error))) => {
                self.check_deadline()?;
                Err(error)
            }
            Some(Ok(SshStderrMessage::Done)) => {
                self.ssh_errors = None;
                Ok(())
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.ssh_errors = None;
                Ok(())
            }
            Some(Err(TryRecvError::Empty)) | None => Ok(()),
        }
    }

    fn receive_message(&mut self) -> std::io::Result<StdoutMessage> {
        loop {
            self.check_deadline()?;
            self.check_ssh_error()?;
            let wait = if self.ssh_errors.is_some() {
                Some(self.deadline.map_or(Duration::from_millis(10), |deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10))
                }))
            } else {
                self.deadline
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            };
            let result = match wait {
                Some(wait) => self.recv.recv_timeout(wait).map_err(Some),
                None => self.recv.recv().map_err(|_| None),
            };
            match result {
                Ok(message) => {
                    self.check_deadline()?;
                    self.check_ssh_error()?;
                    return Ok(message);
                }
                Err(Some(RecvTimeoutError::Timeout)) => {
                    self.check_deadline()?;
                    self.check_ssh_error()?;
                }
                Err(Some(RecvTimeoutError::Disconnected) | None) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "Git transport reader stopped unexpectedly",
                    ));
                }
            }
        }
    }
}

fn supervise_stdout(
    stdout: process::ChildStdout,
    deadline: Option<Instant>,
    read_ssh_errors: bool,
    transfer_limit: Option<FetchTransferLimit>,
) -> std::io::Result<(
    ReadStdoutWithDeadline,
    Option<SyncSender<SshStderrMessage>>,
    JoinHandle<()>,
)> {
    let (send, recv) = mpsc::sync_channel(1);
    let (ssh_error_send, ssh_errors) = if read_ssh_errors {
        let (send, recv) = mpsc::sync_channel(1);
        (Some(send), Some(recv))
    } else {
        (None, None)
    };
    let reader = std::thread::Builder::new()
        .name("read git transport stdout".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut stdout = stdout;
            let mut buffer = vec![0; 8 * 1024];
            loop {
                let remaining = transfer_limit
                    .as_ref()
                    .map(FetchTransferLimit::remaining_bytes);
                let read_capacity = remaining.map_or(buffer.len(), |remaining| {
                    remaining.saturating_add(1).min(buffer.len() as u64) as usize
                });
                let message = match stdout.read(&mut buffer[..read_capacity]) {
                    Ok(0) => StdoutMessage::Eof,
                    Ok(read) => {
                        if let (Some(limit), Some(remaining)) = (&transfer_limit, remaining)
                            && read as u64 > remaining
                        {
                            let allowed = remaining as usize;
                            if allowed > 0 {
                                if let Err(error) = limit.consume(remaining) {
                                    let _ = send.send(StdoutMessage::Error(error));
                                    break;
                                }
                                if send
                                    .send(StdoutMessage::Data(buffer[..allowed].to_vec()))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            let _ = send.send(StdoutMessage::Error(limit.exceeded_error()));
                            break;
                        }
                        if let Some(limit) = &transfer_limit
                            && limit.consume(read as u64).is_err()
                        {
                            let _ = send.send(StdoutMessage::Error(limit.exceeded_error()));
                            break;
                        }
                        StdoutMessage::Data(buffer[..read].to_vec())
                    }
                    Err(error) => StdoutMessage::Error(error),
                };
                let done = matches!(&message, StdoutMessage::Error(_) | StdoutMessage::Eof);
                if send.send(message).is_err() || done {
                    break;
                }
            }
        })?;
    Ok((
        ReadStdoutWithDeadline {
            recv,
            ssh_errors,
            buffered: Vec::new(),
            buffered_offset: 0,
            deadline,
        },
        ssh_error_send,
        reader,
    ))
}

const MAX_SSH_STDERR_LINE_BYTES: usize = 4096;

fn supervise_stderr(
    ssh_kind: ssh::ProgramKind,
    stderr: process::ChildStderr,
    send: SyncSender<SshStderrMessage>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("read git transport stderr".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut stderr = stderr;
            let mut chunk = [0; 1024];
            let mut line = Vec::with_capacity(MAX_SSH_STDERR_LINE_BYTES);
            let mut overlong = false;
            let mut error_reported = false;
            loop {
                let read = match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                for byte in &chunk[..read] {
                    if *byte == b'\n' {
                        report_ssh_error_line(
                            &ssh_kind,
                            &mut line,
                            overlong,
                            &send,
                            &mut error_reported,
                        );
                        overlong = false;
                    } else if !overlong {
                        if line.len() < MAX_SSH_STDERR_LINE_BYTES {
                            line.push(*byte);
                        } else {
                            line.clear();
                            overlong = true;
                        }
                    }
                }
            }
            report_ssh_error_line(&ssh_kind, &mut line, overlong, &send, &mut error_reported);
            let _ = send.send(SshStderrMessage::Done);
        })
}

fn report_ssh_error_line(
    ssh_kind: &ssh::ProgramKind,
    line: &mut Vec<u8>,
    overlong: bool,
    send: &SyncSender<SshStderrMessage>,
    error_reported: &mut bool,
) {
    if !overlong && !line.is_empty() {
        if let Ok(error) = ssh_kind.line_to_err(std::mem::take(line).into()) {
            if !*error_reported {
                let _ = send.send(SshStderrMessage::Error(error));
                *error_reported = true;
            }
        }
    }
    line.clear();
}

impl client::blocking_io::Transport for SpawnProcessOnDemand {
    fn handshake<'a>(
        &mut self,
        service: Service,
        extra_parameters: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<SetServiceResponse<'_>, client::Error> {
        self.cleanup_subprocess();
        let (cmd, ssh_kind, cmd_name) = self.prepare_command(service)?;
        let envs = std::mem::take(&mut self.envs);
        let into_std_command = |mut cmd: gix_command::Prepare| {
            cmd.stdin = Stdio::piped();
            cmd.stdout = Stdio::piped();

            let mut cmd = std::process::Command::from(cmd);
            for env_to_remove in ENV_VARS_TO_REMOVE {
                cmd.env_remove(env_to_remove);
            }
            cmd.envs(envs.iter().map(|(k, v)| (k, v)));
            cmd
        };

        let mut cmd = into_std_command(cmd);
        gix_features::trace::debug!(command = ?cmd, "gix_transport::SpawnProcessOnDemand");
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) if ssh_kind.is_none() && err.kind() == std::io::ErrorKind::NotFound => {
                // The service program wasn't found in `PATH`, but as `git` itself can be found,
                // the service can still be run through it (#2313).
                let (cmd, cmd_name) = self.prepare_fallback_command(service);
                let mut cmd = into_std_command(cmd);
                gix_features::trace::debug!(command = ?cmd, "gix_transport::SpawnProcessOnDemand (fallback)");
                cmd.spawn().map_err(|err| client::Error::InvokeProgram {
                    source: err,
                    command: cmd_name,
                })?
            }
            Err(err) => {
                return Err(client::Error::InvokeProgram {
                    source: err,
                    command: cmd_name,
                });
            }
        };
        self.child = Some(child);
        let child = self.child.as_mut().expect("child was just assigned");
        let stdout = child.stdout.take().expect("stdout configured");
        let stdin = child.stdin.take().expect("stdin configured");
        let stderr = child.stderr.take();
        let deadline = self
            .subprocess_read_timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        let (stdout, ssh_error_send, stdout_reader) = supervise_stdout(
            stdout,
            deadline,
            ssh_kind.is_some(),
            self.fetch_transfer_limit.clone(),
        )
        .map_err(client::Error::Io)?;
        self.stdout_reader_thread = Some(stdout_reader);
        if let Some(ssh_kind) = ssh_kind {
            let stderr = stderr.expect("SSH stderr is piped");
            let ssh_error_send = ssh_error_send.expect("SSH error receiver was created");
            let stderr_reader =
                supervise_stderr(ssh_kind, stderr, ssh_error_send).map_err(client::Error::Io)?;
            self.stderr_reader_thread = Some(stderr_reader);
        }
        self.connection = Some(Connection::new_for_spawned_process(
            Box::new(stdout),
            stdin,
            self.desired_version,
            self.path.clone(),
            self.trace,
        ));
        self.connection
            .as_mut()
            .expect("connection to be there right after setting it")
            .handshake(service, extra_parameters)
    }

    fn request(
        &mut self,
        write_mode: WriteMode,
        on_into_read: MessageKind,
        trace: bool,
    ) -> Result<RequestWriter<'_>, client::Error> {
        self.connection
            .as_mut()
            .ok_or(client::Error::MissingHandshake)?
            .request(write_mode, on_into_read, trace)
    }
}

/// Connect to a locally readable repository at `path` using the given `desired_version`.
/// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
///
/// This will spawn a `git` process locally.
pub fn connect(
    path: impl Into<BString>,
    desired_version: Protocol,
    trace: bool,
) -> Result<SpawnProcessOnDemand, std::convert::Infallible> {
    Ok(SpawnProcessOnDemand::new_local(
        path.into(),
        desired_version,
        trace,
    ))
}

#[cfg(test)]
mod tests {
    mod timeout {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        use crate::{
            Protocol,
            client::blocking_io::{
                FetchTransferLimit, SubprocessReadTimeout,
                file::{
                    ReadStdoutWithDeadline, SpawnProcessOnDemand, SshStderrMessage, StdoutMessage,
                },
                ssh::ProgramKind,
            },
        };

        #[cfg(unix)]
        #[test]
        fn timed_out_stdout_reader_is_joined_after_its_child_is_killed() {
            let mut child = Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("shell is available on Unix");
            let stdout = child.stdout.take().expect("stdout was piped");
            let (mut reader, _, worker) = super::super::supervise_stdout(
                stdout,
                Some(Instant::now() + Duration::from_millis(20)),
                false,
                None,
            )
            .expect("reader thread can start");
            let mut transport =
                SpawnProcessOnDemand::new_local("/unused".into(), Protocol::V2, false);
            transport.child = Some(child);
            transport.stdout_reader_thread = Some(worker);

            let mut byte = [0];
            let error = reader
                .read(&mut byte)
                .expect_err("child does not produce output");
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);

            drop(reader);
            let started = Instant::now();
            drop(transport);
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "dropping a timed-out transport must kill the child and join its reader"
            );
        }

        #[cfg(unix)]
        #[test]
        fn stdout_transfer_limit_reads_only_one_byte_past_the_budget() {
            let mut child = Command::new("sh")
                .args(["-c", "printf abcdef"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("shell is available on Unix");
            let stdout = child.stdout.take().expect("stdout was piped");
            let limit = FetchTransferLimit::new(4);
            let (mut reader, _, worker) = super::super::supervise_stdout(
                stdout,
                Some(Instant::now() + Duration::from_secs(1)),
                false,
                Some(limit.clone()),
            )
            .expect("reader thread can start");
            let mut output = Vec::new();

            let error = reader
                .read_to_end(&mut output)
                .expect_err("the fifth response byte exceeds the budget");
            worker.join().expect("stdout worker exits");
            child.wait().expect("child exits");

            assert_eq!(output, b"abcd");
            assert_eq!(limit.consumed_bytes(), 4);
            assert_eq!(
                error
                    .get_ref()
                    .and_then(|source| source
                        .downcast_ref::<crate::client::blocking_io::FetchTransferLimitExceeded>())
                    .map(|source| source.limit_bytes),
                Some(4)
            );
        }

        #[cfg(unix)]
        #[test]
        fn cleanup_subprocess_stops_a_failed_handshake_before_retry() {
            let mut child = Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("shell is available on Unix");
            let stdout = child.stdout.take().expect("stdout was piped");
            let stderr = child.stderr.take().expect("stderr was piped");
            let (stdout, ssh_error_send, stdout_reader) =
                super::super::supervise_stdout(stdout, None, true, None)
                    .expect("stdout reader thread can start");
            let stderr_reader = super::super::supervise_stderr(
                ProgramKind::Ssh,
                stderr,
                ssh_error_send.expect("SSH error channel was created"),
            )
            .expect("stderr reader thread can start");
            let mut transport =
                SpawnProcessOnDemand::new_local("/unused".into(), Protocol::V2, false);
            transport.child = Some(child);
            transport.stdout_reader_thread = Some(stdout_reader);
            transport.stderr_reader_thread = Some(stderr_reader);
            drop(stdout);

            let started = Instant::now();
            transport.cleanup_subprocess();

            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(transport.child.is_none());
            assert!(transport.connection.is_none());
            assert!(transport.stdout_reader_thread.is_none());
            assert!(transport.stderr_reader_thread.is_none());
        }

        #[cfg(unix)]
        #[test]
        fn ssh_stderr_is_drained_after_a_classified_error() {
            let mut child = Command::new("sh")
                .args([
                    "-c",
                    "printf 'ssh: connect to host host port 22: No route to host\\n' >&2; dd if=/dev/zero bs=4096 count=256 >&2 2>/dev/null || exit 9; printf drained",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("shell is available on Unix");
            let stdout = child.stdout.take().expect("stdout was piped");
            let stderr = child.stderr.take().expect("stderr was piped");
            let (send, recv) = mpsc::sync_channel(1);
            let reader = super::super::supervise_stderr(ProgramKind::Ssh, stderr, send)
                .expect("stderr reader thread can start");

            let wait_until = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = child.try_wait().expect("child status can be read") {
                    break status;
                }
                if Instant::now() >= wait_until {
                    child.kill().ok();
                    child.wait().ok();
                    reader
                        .join()
                        .expect("stderr reader exits after child is killed");
                    panic!("child blocked because SSH stderr was not drained");
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let error = recv.recv().expect("classified SSH error is retained");
            reader.join().expect("stderr reader exits");
            assert!(status.success(), "child must finish writing stderr");

            let mut output = Vec::new();
            stdout
                .take(16)
                .read_to_end(&mut output)
                .expect("stdout can be read");
            assert_eq!(output, b"drained");
            assert!(matches!(
                &error,
                SshStderrMessage::Error(error)
                    if error.kind() == std::io::ErrorKind::NotFound
            ));
        }

        #[test]
        fn expired_deadline_rejects_already_buffered_stdout() {
            let (send, recv) = mpsc::sync_channel(1);
            send.send(StdoutMessage::Data(vec![b'x']))
                .expect("reader is alive");
            let mut reader = ReadStdoutWithDeadline {
                recv,
                ssh_errors: None,
                buffered: Vec::new(),
                buffered_offset: 0,
                deadline: Some(
                    Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .unwrap_or_else(Instant::now),
                ),
            };

            let mut byte = [0];
            let error = reader
                .read(&mut byte)
                .expect_err("expired deadline must take precedence over queued data");

            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        }

        #[test]
        fn eof_waits_for_delayed_ssh_stderr_classification() {
            let (stdout_send, stdout_recv) = mpsc::sync_channel(1);
            stdout_send
                .send(StdoutMessage::Eof)
                .expect("stdout reader is alive");
            let (ssh_send, ssh_recv) = mpsc::sync_channel(1);
            let stderr_reader = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                ssh_send
                    .send(SshStderrMessage::Error(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "SSH connection failed",
                    )))
                    .expect("SSH error receiver is alive");
                ssh_send
                    .send(SshStderrMessage::Done)
                    .expect("SSH error receiver is alive");
            });
            let mut reader = ReadStdoutWithDeadline {
                recv: stdout_recv,
                ssh_errors: Some(ssh_recv),
                buffered: Vec::new(),
                buffered_offset: 0,
                deadline: Some(Instant::now() + Duration::from_secs(1)),
            };

            let mut byte = [0];
            let error = reader
                .read(&mut byte)
                .expect_err("delayed SSH connection failure is retained");

            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
            stderr_reader.join().expect("stderr worker exits");
        }

        #[test]
        fn eof_does_not_accept_an_ssh_error_after_the_deadline() {
            let (stdout_send, stdout_recv) = mpsc::sync_channel(1);
            stdout_send
                .send(StdoutMessage::Eof)
                .expect("stdout reader is alive");
            let (ssh_send, ssh_recv) = mpsc::sync_channel(1);
            let stderr_reader = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                let _ = ssh_send.send(SshStderrMessage::Error(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "late SSH connection failure",
                )));
                let _ = ssh_send.send(SshStderrMessage::Done);
            });
            let mut reader = ReadStdoutWithDeadline {
                recv: stdout_recv,
                ssh_errors: Some(ssh_recv),
                buffered: Vec::new(),
                buffered_offset: 0,
                deadline: Some(Instant::now() + Duration::from_millis(20)),
            };

            let mut byte = [0];
            let error = reader
                .read(&mut byte)
                .expect_err("the read deadline takes precedence over a late SSH error");

            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            drop(reader);
            stderr_reader.join().expect("stderr worker exits");
        }

        #[cfg(unix)]
        #[test]
        fn ssh_stderr_retains_only_the_known_error_kind() {
            let mut child = Command::new("sh")
                .args([
                    "-c",
                    "printf 'ssh: Could not resolve hostname host: nodename nor servname provided, or not known\\n' >&2",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("shell is available on Unix");
            let stderr = child.stderr.take().expect("stderr was piped");
            let (send, recv) = mpsc::sync_channel(1);
            let reader = super::super::supervise_stderr(ProgramKind::Ssh, stderr, send)
                .expect("stderr reader thread can start");
            child.wait().expect("child exits");

            let error = recv
                .recv_timeout(Duration::from_secs(1))
                .expect("known SSH error is retained");
            reader.join().expect("stderr reader exits");
            let SshStderrMessage::Error(error) = error else {
                panic!("expected classified SSH error");
            };
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
            assert_eq!(
                error.to_string(),
                "ssh: Could not resolve hostname host: nodename nor servname provided, or not known"
            );
        }

        #[test]
        fn subprocess_read_timeout_is_taken_from_transport_options() {
            let mut transport =
                SpawnProcessOnDemand::new_local("/unused".into(), Protocol::V2, false);
            let options = SubprocessReadTimeout::new(Duration::from_secs(7));

            crate::client::TransportWithoutIO::configure(&mut transport, &options)
                .expect("configuration is valid");

            assert_eq!(
                transport.subprocess_read_timeout,
                Some(Duration::from_secs(7))
            );
        }
    }

    mod local {
        use crate::{Protocol, Service, client::blocking_io::file::SpawnProcessOnDemand};

        #[test]
        fn fallback_command_prefers_the_core_dir_program_and_can_always_run_git_itself() {
            let transport =
                SpawnProcessOnDemand::new_local("/repo/path".into(), Protocol::V2, false);
            let (cmd, name) = transport.prepare_fallback_command(Service::UploadPack);

            assert!(
                !cmd.use_shell,
                "a shell is never used to run the local service program"
            );
            assert_eq!(
                cmd.args.last().map(std::path::Path::new),
                Some(std::path::Path::new("/repo/path")),
                "the repository path is the final argument"
            );
            match gix_path::env::core_dir_program(Service::UploadPack.as_str()) {
                Some(program) => {
                    assert_eq!(
                        std::path::Path::new(&cmd.command),
                        program,
                        "the program shipped with Git in its `--exec-path` is used directly"
                    );
                    assert!(
                        std::path::Path::new(&cmd.command).is_absolute(),
                        "which also means it's an absolute path"
                    );
                    assert_eq!(
                        cmd.args.len(),
                        1,
                        "there is no sub-command, just the repo path"
                    );
                    assert_eq!(name, program.into_os_string());
                }
                None => {
                    assert_eq!(
                        std::path::Path::new(&cmd.command),
                        gix_path::env::exe_invocation(),
                        "without it, `git` itself runs the service as subcommand"
                    );
                    assert_eq!(
                        cmd.args.first().and_then(|arg| arg.to_str()),
                        Some("upload-pack"),
                        "the sub-command is passed as separate argument, without the 'git-' prefix"
                    );
                    assert_eq!(cmd.args.len(), 2, "sub-command and repo path");
                }
            }
        }
    }

    mod ssh {
        mod connect {
            use crate::{Protocol, client::blocking_io::ssh};

            #[test]
            fn path() {
                for (url, expected) in [
                    ("ssh://host.xy/~/repo", "~/repo"),
                    ("ssh://host.xy/~username/repo", "~username/repo"),
                    ("user@host.xy:/username/repo", "/username/repo"),
                    ("user@host.xy:username/repo", "username/repo"),
                    ("user@host.xy:../username/repo", "../username/repo"),
                    ("user@host.xy:~/repo", "~/repo"),
                ] {
                    let url = gix_url::parse(url).expect("valid url");
                    let cmd = ssh::connect(url, Protocol::V1, Default::default(), false)
                        .expect("parse success");
                    assert_eq!(
                        cmd.path, expected,
                        "the path will be substituted by the remote shell"
                    );
                }
            }

            #[test]
            fn upload_pack_invocation_preserves_scp_like_path_distinction() {
                for (url, expected) in [
                    (
                        "git@forge.com:/path/repo",
                        &["ssh", "git@forge.com", "git-upload-pack", "'/path/repo'"][..],
                    ),
                    (
                        "git@forge.com:path/repo",
                        &["ssh", "git@forge.com", "git-upload-pack", "'path/repo'"][..],
                    ),
                    (
                        "ssh://git@forge.com/path/repo",
                        &["ssh", "git@forge.com", "git-upload-pack", "'/path/repo'"][..],
                    ),
                ] {
                    let url = gix_url::parse(url).expect("valid url");
                    let cmd = ssh::connect(url, Protocol::V1, Default::default(), false)
                        .expect("parse success");
                    assert_eq!(
                        command_and_args(
                            cmd.prepare_command(crate::Service::UploadPack)
                                .expect("valid command")
                                .0
                        ),
                        expected,
                        "the remote shell command must match Git's parsed repository path"
                    );
                }
            }

            #[test]
            fn ambiguous_host_disallowed() {
                for url in [
                    "ssh://-oProxyCommand=open$IFS-aCalculator/foo",
                    "user@-oProxyCommand=open$IFS-aCalculator:username/repo",
                ] {
                    let url = gix_url::parse(url).expect("valid url");
                    let options = ssh::connect::Options {
                        command: Some("unrecognized".into()),
                        disallow_shell: false,
                        kind: None,
                    };
                    assert!(matches!(
                        ssh::connect(url, Protocol::V1, options, false),
                        Err(ssh::Error::AmbiguousHostName { host }) if host == "-oProxyCommand=open$IFS-aCalculator",
                    ));
                }
            }

            fn command_and_args(cmd: gix_command::Prepare) -> Vec<String> {
                let cmd = std::process::Command::from(cmd);
                std::iter::once(cmd.get_program())
                    .chain(cmd.get_args())
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect()
            }
        }
    }
}
