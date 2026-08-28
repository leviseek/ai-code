use std::io;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ProcessOutput {
    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

pub fn run_capture(
    program: &str,
    args: &[String],
    working_directory: &Path,
) -> io::Result<ProcessOutput> {
    let output = Command::new(program)
        .args(args)
        .current_dir(working_directory)
        .output()?;
    Ok(ProcessOutput {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub fn run_capture_timeout(
    program: &str,
    args: &[String],
    working_directory: &Path,
    timeout: std::time::Duration,
) -> io::Result<ProcessOutput> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(working_directory)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() >= timeout {
            child.kill()?;
            let output = child.wait_with_output()?;
            return Ok(ProcessOutput {
                exit_code: None,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: format!(
                    "{}process timed out after {:?}",
                    String::from_utf8_lossy(&output.stderr),
                    timeout
                ),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let output = child.wait_with_output()?;
    Ok(ProcessOutput {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Identifies the standard stream an [`OutputChunk`] was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

/// One chunk of raw bytes streamed from a running process's stdout or stderr.
///
/// Chunk boundaries are arbitrary read boundaries; a UTF-8 character may be
/// split across chunks. Use [`OutputChunk::text`] only for display, or
/// accumulate the raw bytes and decode once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputChunk {
    pub kind: StreamKind,
    pub data: Vec<u8>,
}

impl OutputChunk {
    /// Lossily decodes this chunk for display purposes.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// A spawned process whose stdout and stderr are streamed as chunks.
///
/// Each stream is drained by its own dedicated reader thread into a shared
/// channel, so both streams keep making progress even while the other is
/// idle, and the child can never block on a full pipe.
///
/// Dropping the handle kills the child if it is still running, reaps it and
/// joins the reader threads, so no process, zombie or thread outlives the
/// handle.
pub struct RunningProcess {
    child: Option<Child>,
    chunks: Option<mpsc::Receiver<OutputChunk>>,
    threads: Vec<JoinHandle<()>>,
}

impl RunningProcess {
    /// The child's OS process id, if the process has not been waited on yet.
    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Returns the next chunk if one is already buffered; never blocks.
    ///
    /// `Ok(None)` means either no chunk has arrived yet or both streams are
    /// exhausted. Use [`RunningProcess::next_chunk`] to wait for the next
    /// chunk.
    pub fn try_next_chunk(&mut self) -> io::Result<Option<OutputChunk>> {
        let chunks = self.chunks.as_ref().ok_or_else(already_waited)?;
        match chunks.try_recv() {
            Ok(chunk) => Ok(Some(chunk)),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => Ok(None),
        }
    }

    /// Blocks until the next chunk arrives or both streams reach EOF.
    ///
    /// Returns `Ok(None)` once every reader thread has finished, i.e. both
    /// stdout and stderr are fully drained.
    pub fn next_chunk(&mut self) -> io::Result<Option<OutputChunk>> {
        let chunks = self.chunks.as_ref().ok_or_else(already_waited)?;
        match chunks.recv() {
            Ok(chunk) => Ok(Some(chunk)),
            Err(mpsc::RecvError) => Ok(None),
        }
    }

    /// Terminates the child if it is still running.
    ///
    /// This is a no-op if the process has already been waited on or exited on
    /// its own; it does not drain the stream channel.
    pub fn kill(&mut self) -> io::Result<()> {
        match self.child.as_mut() {
            Some(child) => child.kill(),
            None => Ok(()),
        }
    }

    /// Drains all remaining output, waits for the child to exit and reaps it.
    ///
    /// The channel is drained to exhaustion *before* waiting on the child, so
    /// the reader threads never block on a full channel and the child can
    /// always flush its pipes; only then is the child waited on. May only be
    /// called once; later calls return an error.
    pub fn wait(&mut self) -> io::Result<ProcessOutput> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let chunks = self.chunks.as_ref().ok_or_else(already_waited)?;
        while let Ok(chunk) = chunks.recv() {
            match chunk.kind {
                StreamKind::Stdout => stdout.extend_from_slice(&chunk.data),
                StreamKind::Stderr => stderr.extend_from_slice(&chunk.data),
            }
        }
        // The channel is closed, so both readers hit EOF and the child closed
        // its pipes; it is safe to wait on it now.
        let mut child = self.child.take().ok_or_else(already_waited)?;
        let status = child.wait()?;
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
        Ok(ProcessOutput {
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        // Best-effort kill + reap so no child process or zombie survives the
        // handle being dropped.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Dropping the receiver unblocks any reader thread stuck sending into
        // a full channel; joining then reaps both reader threads.
        drop(self.chunks.take());
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Spawns `program` with piped stdout/stderr and starts streaming both.
///
/// Each stream is read by an independent dedicated thread; chunks arrive on
/// the [`RunningProcess`] handle in whatever order they were read.
pub fn spawn_streaming(
    program: &str,
    args: &[String],
    working_directory: &Path,
) -> io::Result<RunningProcess> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(working_directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let (sender, receiver) = mpsc::channel();
    let stdout_thread = spawn_reader(
        "nextide-kernel-stdout",
        stdout,
        StreamKind::Stdout,
        sender.clone(),
    );
    let stderr_thread = spawn_reader("nextide-kernel-stderr", stderr, StreamKind::Stderr, sender);

    let mut threads = Vec::with_capacity(2);
    let mut failed: Option<io::Error> = None;
    for result in [stdout_thread, stderr_thread] {
        match result {
            Ok(handle) => threads.push(handle),
            Err(err) => failed = Some(err),
        }
    }
    if let Some(err) = failed {
        // Reader thread creation failed; tear the child down so nothing leaks.
        let _ = child.kill();
        let _ = child.wait();
        for handle in threads {
            let _ = handle.join();
        }
        return Err(err);
    }

    Ok(RunningProcess {
        child: Some(child),
        chunks: Some(receiver),
        threads,
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    name: &str,
    mut reader: R,
    kind: StreamKind,
    sender: mpsc::Sender<OutputChunk>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break, // EOF: this stream is finished.
                    Ok(n) => {
                        if sender
                            .send(OutputChunk {
                                kind,
                                data: buffer[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break; // Receiver dropped; stop streaming.
                        }
                    }
                    Err(_) => break, // Broken pipe; stream is over.
                }
            }
        })
}

fn already_waited() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "process has already been waited on",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_timeout_without_hanging() {
        let result = if cfg!(windows) {
            run_capture_timeout(
                "ping",
                &["127.0.0.1".into(), "-n".into(), "5".into()],
                Path::new("."),
                std::time::Duration::from_millis(20),
            )
        } else {
            run_capture_timeout(
                "sh",
                &["-c".into(), "sleep 1".into()],
                Path::new("."),
                std::time::Duration::from_millis(20),
            )
        }
        .unwrap();
        assert_eq!(result.exit_code, None);
        assert!(!result.is_success());
        assert!(result.stderr.contains("timed out"));
    }

    #[test]
    fn captures_successful_process_output() {
        let result = if cfg!(windows) {
            run_capture(
                "cmd.exe",
                &["/C".into(), "echo hello".into()],
                Path::new("."),
            )
        } else {
            run_capture("sh", &["-c".into(), "printf hello".into()], Path::new("."))
        }
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.is_success());
        assert_eq!(result.combined(), result.stdout.clone() + &result.stderr);
        assert!(result.stdout.contains("hello"));
    }

    #[test]
    fn streams_successful_process_output() {
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".into(), "echo hello".into()])
        } else {
            ("sh", vec!["-c".into(), "printf hello".into()])
        };
        let mut proc = spawn_streaming(program, &args, Path::new(".")).unwrap();
        let mut stdout = Vec::new();
        while let Some(chunk) = proc.next_chunk().unwrap() {
            assert_eq!(chunk.kind, StreamKind::Stdout);
            stdout.extend_from_slice(&chunk.data);
        }
        assert!(
            String::from_utf8_lossy(&stdout).contains("hello"),
            "streamed stdout was {:?}",
            String::from_utf8_lossy(&stdout)
        );
        let output = proc.wait().unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(output.is_success());
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn streams_stdout_and_stderr_independently() {
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec!["/C".into(), "echo out && echo err 1>&2".into()],
            )
        } else {
            ("sh", vec!["-c".into(), "printf out; printf err >&2".into()])
        };
        let mut proc = spawn_streaming(program, &args, Path::new(".")).unwrap();
        let mut saw_stdout = false;
        let mut saw_stderr = false;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Some(chunk) = proc.next_chunk().unwrap() {
            match chunk.kind {
                StreamKind::Stdout => {
                    saw_stdout = true;
                    stdout.extend_from_slice(&chunk.data);
                }
                StreamKind::Stderr => {
                    saw_stderr = true;
                    stderr.extend_from_slice(&chunk.data);
                }
            }
        }
        let output = proc.wait().unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(saw_stdout, "stdout stream never produced a chunk");
        assert!(saw_stderr, "stderr stream never produced a chunk");
        assert!(
            String::from_utf8_lossy(&stdout).contains("out"),
            "streamed stdout was {:?}",
            String::from_utf8_lossy(&stdout)
        );
        assert!(
            String::from_utf8_lossy(&stderr).contains("err"),
            "streamed stderr was {:?}",
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn wait_drains_all_output_before_reaping() {
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".into(), "echo hello".into()])
        } else {
            ("sh", vec!["-c".into(), "printf hello".into()])
        };
        let mut proc = spawn_streaming(program, &args, Path::new(".")).unwrap();
        // No chunks are consumed beforehand: wait() must drain the entire
        // channel first, then wait for and reap the child.
        let output = proc.wait().unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(output.is_success());
        assert!(output.stdout.contains("hello"));
        assert!(output.stderr.is_empty());
        // The child handle is consumed by wait(); a second call must fail.
        assert_eq!(proc.wait().unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn try_next_chunk_does_not_block_and_kill_terminates() {
        let (program, args) = if cfg!(windows) {
            (
                "powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Start-Sleep -Seconds 30".into(),
                ],
            )
        } else {
            ("sh", vec!["-c".into(), "exec sleep 30".into()])
        };
        let mut proc = spawn_streaming(program, &args, Path::new(".")).unwrap();
        // The child never writes, so a non-blocking poll must return None
        // immediately instead of waiting for output.
        assert_eq!(proc.try_next_chunk().unwrap(), None);
        proc.kill().unwrap();
        let output = proc.wait().unwrap();
        assert_ne!(output.exit_code, Some(0));
        assert!(!output.is_success());
    }

    #[test]
    fn drop_kills_and_reaps_a_running_process() {
        let (program, args) = if cfg!(windows) {
            (
                "powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Start-Sleep -Seconds 30".into(),
                ],
            )
        } else {
            ("sh", vec!["-c".into(), "exec sleep 30".into()])
        };
        let proc = spawn_streaming(program, &args, Path::new(".")).unwrap();
        let pid = proc.id().unwrap();
        // Give the child a moment to fully start before killing it via drop.
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(proc);
        assert!(
            !pid_is_running(pid),
            "process {pid} should have been killed by dropping the handle"
        );
    }

    #[cfg(windows)]
    fn pid_is_running(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        let output = std::process::Command::new("tasklist")
            .args(["/FI", filter.as_str()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().any(|line| {
            line.split_whitespace()
                .any(|token| token == pid.to_string())
        })
    }

    #[cfg(not(windows))]
    fn pid_is_running(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}
