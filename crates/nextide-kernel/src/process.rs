use std::io;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;

/// How often [`RunningProcess::wait`] polls the child while draining the
/// stream channel, so a descendant holding the pipes open is detected and
/// reaped shortly after the direct child exits. Also the sleep granularity
/// used by the deadline-bounded callers while waiting for a child that has
/// already closed its streams.
pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

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
    // Built on the streaming primitive so both the timeout and the completion
    // path inherit its process-tree reaping: a descendant that keeps the
    // stdout/stderr pipes open after the direct child exits cannot stall the
    // wait forever.
    let mut proc = spawn_streaming(program, args, working_directory)?;
    let started = std::time::Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut timed_out = false;
    let mut streams_closed = false;
    loop {
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            timed_out = true;
            break;
        }
        let remaining = timeout - elapsed;
        if streams_closed {
            // Both streams reached EOF, but the child may still be running
            // (e.g. it closed its stdout/stderr without exiting). Poll the
            // child itself until the deadline so the timeout still applies,
            // instead of blocking on wait() until the process exits on its
            // own — possibly forever.
            if proc.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(remaining.min(POLL_INTERVAL));
            continue;
        }
        match proc.next_chunk_timeout(remaining)? {
            StreamWait::Chunk(chunk) => match chunk.kind {
                StreamKind::Stdout => stdout.extend_from_slice(&chunk.data),
                StreamKind::Stderr => stderr.extend_from_slice(&chunk.data),
            },
            StreamWait::Closed => streams_closed = true,
            StreamWait::Timeout => {
                timed_out = true;
                break;
            }
        }
    }
    if timed_out {
        // Reap the whole tree so the pipes close, then collect whatever the
        // reader threads had buffered before they hit EOF.
        let _ = proc.kill();
        while let Some(chunk) = proc.next_chunk()? {
            match chunk.kind {
                StreamKind::Stdout => stdout.extend_from_slice(&chunk.data),
                StreamKind::Stderr => stderr.extend_from_slice(&chunk.data),
            }
        }
        let _ = proc.wait()?;
        let mut message = String::from_utf8_lossy(&stderr).into_owned();
        message.push_str(&format!("process timed out after {:?}", timeout));
        Ok(ProcessOutput {
            exit_code: None,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: message,
        })
    } else {
        Ok(proc.wait()?)
    }
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
/// split across chunks. Use [`OutputChunk::text`] only for lossy per-chunk
/// display; to reconstruct the exact stream text, feed the raw bytes into a
/// [`Utf8Decoder`] (or accumulate the raw bytes and decode once).
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

/// Incrementally decodes UTF-8 text from arbitrarily chunked byte slices.
///
/// Chunk boundaries are read boundaries, not character boundaries, so a
/// multi-byte character can be split across chunks. [`Utf8Decoder::feed`]
/// buffers any incomplete trailing sequence and returns only the text that is
/// complete so far; feeding every chunk of a byte stream in order and
/// concatenating the results reproduces the stream's text exactly, with no
/// replacement characters for characters that merely spanned a chunk
/// boundary. [`Utf8Decoder::finish`] flushes the remaining buffer, lossily
/// replacing a sequence that was genuinely truncated at end of stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    /// Creates an empty decoder with no buffered bytes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next byte chunk and returns the decoded text so far.
    ///
    /// A trailing multi-byte sequence that is not yet complete is buffered
    /// and its bytes are returned by a later [`Utf8Decoder::feed`] or
    /// [`Utf8Decoder::finish`] call, so no replacement character is produced
    /// for a character that merely spanned a chunk boundary. Bytes that are
    /// genuinely not UTF-8 are replaced with U+FFFD as in
    /// [`String::from_utf8_lossy`].
    pub fn feed(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut decoded = String::with_capacity(self.pending.len());
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    decoded.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    decoded.push_str(
                        std::str::from_utf8(&self.pending[..valid])
                            .expect("bytes before an invalid sequence are valid UTF-8"),
                    );
                    match error.error_len() {
                        Some(len) => {
                            // A genuinely invalid byte sequence: replace it
                            // and keep decoding the remainder.
                            decoded.push('\u{FFFD}');
                            self.pending.drain(..valid + len);
                        }
                        None => {
                            // The tail is an incomplete multi-byte sequence;
                            // hold it until the next chunk completes it.
                            self.pending.drain(..valid);
                            break;
                        }
                    }
                }
            }
        }
        decoded
    }

    /// Flushes any bytes still buffered, e.g. because the stream ended in the
    /// middle of a multi-byte character.
    ///
    /// Returns the remaining text lossily decoded, so a genuinely truncated
    /// final sequence becomes U+FFFD exactly as it would with
    /// [`String::from_utf8_lossy`] over the whole stream.
    pub fn finish(&mut self) -> String {
        let pending = std::mem::take(&mut self.pending);
        String::from_utf8_lossy(&pending).into_owned()
    }
}

/// Outcome of a deadline-bounded wait for the next [`OutputChunk`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamWait {
    /// A chunk arrived before the deadline.
    Chunk(OutputChunk),
    /// Both streams reached EOF; no further chunk will ever arrive.
    ///
    /// This does **not** imply the process has exited: a process may close
    /// its stdout/stderr (or be replaced by one that does) while still
    /// running. Use [`RunningProcess::try_wait`] or [`RunningProcess::wait`]
    /// to determine whether it has actually exited.
    Closed,
    /// No chunk arrived before the deadline; the process may still be running.
    Timeout,
}

/// A spawned process whose stdout and stderr are streamed as chunks.
///
/// Each stream is drained by its own dedicated reader thread into a shared
/// channel, so both streams keep making progress even while the other is
/// idle, and the child can never block on a full pipe.
///
/// Waiting on or dropping the handle reaps the *whole process tree*: the
/// child is spawned in its own process group on Unix (killed wholesale via
/// `kill -KILL -<pgid>`), and Windows reaps descendants with
/// `taskkill /PID <pid> /T /F` (falling back to a descendant walk when the
/// direct child already exited). A grandchild that inherits the stdout/stderr
/// pipes can therefore not stall the wait or the reader-thread joins forever.
/// Dropping the handle kills the child if it is still running, reaps it and
/// joins the reader threads, so no process, zombie or thread outlives the
/// handle.
pub struct RunningProcess {
    child: Option<Child>,
    /// The child's exit status once [`RunningProcess::try_wait`] has observed
    /// it (on Unix `try_wait` reaps the child, so the status must be cached
    /// for [`RunningProcess::wait`] to reuse instead of waiting again).
    exit_status: Option<std::process::ExitStatus>,
    chunks: Option<mpsc::Receiver<OutputChunk>>,
    threads: Vec<JoinHandle<()>>,
}

impl RunningProcess {
    /// The child's OS process id, if the process has not been waited on yet.
    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Returns the child's exit status if it has already exited, without
    /// blocking.
    ///
    /// This is how a caller keeps honoring a deadline once the streams have
    /// closed: [`StreamWait::Closed`] only means no more output will arrive,
    /// not that the process exited, so poll this instead of calling
    /// [`RunningProcess::wait`] when the process may still be running. The
    /// first call that observes an exit reaps the child; the status is cached,
    /// so later calls and [`RunningProcess::wait`] return it again. Returns
    /// `Ok(None)` while the child is still running (or after it has been
    /// waited on).
    pub fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.exit_status
    }

    pub fn is_running(&mut self) -> io::Result<bool> {
        Ok(self.try_wait()?.is_none() && self.child.is_some())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        match self.child.as_mut() {
            Some(child) => {
                let status = child.try_wait()?;
                self.exit_status = status;
                Ok(status)
            }
            None => Ok(None),
        }
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

    /// Blocks until the next chunk arrives, both streams reach EOF, or
    /// `timeout` elapses, distinguishing all three outcomes.
    ///
    /// Use this to enforce a deadline while streaming: [`StreamWait::Timeout`]
    /// means no output arrived in time and the process may still be running
    /// (kill it explicitly if it must not outlive the wait).
    /// [`StreamWait::Closed`] means no more output will ever arrive but does
    /// *not* mean the process has exited; check
    /// [`RunningProcess::try_wait`] before treating the wait as complete.
    pub fn next_chunk_timeout(&mut self, timeout: std::time::Duration) -> io::Result<StreamWait> {
        let chunks = self.chunks.as_ref().ok_or_else(already_waited)?;
        match chunks.recv_timeout(timeout) {
            Ok(chunk) => Ok(StreamWait::Chunk(chunk)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(StreamWait::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(StreamWait::Closed),
        }
    }

    /// Terminates the child and, as far as possible, its descendants.
    ///
    /// Windows reaps the whole tree with `taskkill /PID <pid> /T /F` (or a
    /// descendant walk when the direct child already exited); Unix kills the
    /// child's process group. Descendants that escaped the tree (e.g.
    /// daemonized processes) cannot be reached. This is a no-op if the process
    /// has already been waited on; it does not drain the stream channel.
    pub fn kill(&mut self) -> io::Result<()> {
        self.kill_tree()
    }

    /// Kills the child and its descendants (best effort).
    fn kill_tree(&mut self) -> io::Result<()> {
        match self.child.as_mut() {
            Some(child) => kill_child_tree(child),
            None => Ok(()),
        }
    }

    /// Drains all remaining output, waits for the child to exit and reaps it.
    ///
    /// The channel is drained to exhaustion *before* waiting on the child, so
    /// the reader threads never block on a full channel and the child can
    /// always flush its pipes; only then is the child waited on. While
    /// draining, the direct child is polled: once it has exited but its
    /// streams are still open, a descendant must be holding the pipes, so the
    /// whole process tree is killed (best effort) to unblock the readers
    /// instead of hanging forever. If the streams closed early — the child
    /// exited, or it closed its stdout/stderr while still running — this
    /// blocks until the child actually exits, so deadline-bounded callers
    /// should poll [`RunningProcess::try_wait`] instead of calling this while
    /// the child may still be alive. May only be called once; later calls
    /// return an error.
    pub fn wait(&mut self) -> io::Result<ProcessOutput> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if self.chunks.is_none() {
            return Err(already_waited());
        }
        let mut tree_killed = false;
        loop {
            let chunks = self.chunks.as_ref().expect("checked above");
            match chunks.recv_timeout(POLL_INTERVAL) {
                Ok(chunk) => match chunk.kind {
                    StreamKind::Stdout => stdout.extend_from_slice(&chunk.data),
                    StreamKind::Stderr => stderr.extend_from_slice(&chunk.data),
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !tree_killed && self.try_wait()?.is_some() {
                        // The direct child exited but its streams are still
                        // open: a descendant is holding the pipes. Reap the
                        // tree so the reader threads hit EOF instead of
                        // blocking forever.
                        let _ = self.kill_tree();
                        tree_killed = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let mut child = self.child.take().ok_or_else(already_waited)?;
        let status = match self.exit_status.take() {
            Some(status) => status,
            None => child.wait()?,
        };
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
        // Best-effort tree kill + reap so no descendant can keep the pipes
        // open and stall the reader-thread joins below.
        if self.child.is_some() {
            let _ = self.kill_tree();
        }
        if let Some(mut child) = self.child.take() {
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
/// the [`RunningProcess`] handle in whatever order they were read. The child
/// is spawned in its own process group on Unix so the whole tree can be
/// reaped on demand; Windows reaps trees with `taskkill /T`.
pub fn spawn_streaming(
    program: &str,
    args: &[String],
    working_directory: &Path,
) -> io::Result<RunningProcess> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(working_directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group (pgid == child pid) so `kill -KILL -<pid>` can
        // reap the whole tree, not just the direct child.
        command.process_group(0);
    }
    let mut child = command.spawn()?;
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
        // Reader thread creation failed; tear the whole tree down so nothing
        // leaks and a descendant holding the pipes cannot stall the joins.
        let _ = kill_child_tree(&mut child);
        let _ = child.wait();
        for handle in threads {
            let _ = handle.join();
        }
        return Err(err);
    }

    Ok(RunningProcess {
        child: Some(child),
        exit_status: None,
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

/// Kills `child` and its descendants so no descendant can keep the
/// stdout/stderr pipes open and stall a subsequent wait.
///
/// Windows: `taskkill /PID <pid> /T /F` (the same approach as
/// [`crate::platform::PlatformServices::terminate`]); if the direct child has
/// already exited — taskkill cannot walk a dead root's tree — a PowerShell
/// CIM walk kills every live descendant by `ParentProcessId`. Unix: the child
/// was spawned in its own process group, so `kill -KILL -<pgid>` reaps the
/// group; `setpgid` runs in the child around exec, so the kill is retried
/// briefly to close the race window. Both fall back to a plain `Child::kill`.
#[cfg(windows)]
fn kill_child_tree(child: &mut Child) -> io::Result<()> {
    let pid = child.id();
    let taskkill = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status();
    if matches!(taskkill, Ok(status) if status.success()) {
        return Ok(());
    }
    // The direct child may have already exited, in which case taskkill cannot
    // reconstruct its tree; descendants still record the dead parent's PID,
    // so walk the process snapshot and kill each of them.
    let script = format!(
        "$root = {pid}; $targets = New-Object System.Collections.ArrayList; \
         $q = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue; \
         function Walk($p) {{ foreach ($c in $q) {{ \
         if ($c.ParentProcessId -eq $p) {{ [void]$targets.Add([int]$c.ProcessId); Walk $c.ProcessId }} }} }}; \
         Walk $root; foreach ($t in $targets) {{ Stop-Process -Id $t -Force -ErrorAction SilentlyContinue }}"
    );
    let walked = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script.as_str()])
        .status();
    if matches!(walked, Ok(status) if status.success()) {
        return Ok(());
    }
    child.kill()
}

#[cfg(not(windows))]
fn kill_child_tree(child: &mut Child) -> io::Result<()> {
    let pid = child.id();
    // The child was spawned in its own process group (pgid == pid), so a
    // negative pid signal reaches every descendant that stayed in the group.
    // `setpgid` runs in the child around exec, so retry briefly to close the
    // window where the group does not exist yet.
    let group = format!("-{pid}");
    let mut last_err: Option<io::Error> = None;
    for _ in 0..3 {
        match std::process::Command::new("kill")
            .args(["-KILL", &group])
            .status()
        {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => {
                last_err = Some(io::Error::other(format!(
                    "kill -KILL {group} did not succeed"
                )));
            }
            Err(err) => last_err = Some(err),
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // Fallback: kill just the direct child (covers the setpgid race and hosts
    // without a `kill` binary in PATH).
    match child.kill() {
        Ok(()) => Ok(()),
        Err(err) => Err(last_err.unwrap_or(err)),
    }
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
    fn decoder_passes_through_complete_chunks() {
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"hello"), "hello");
        assert_eq!(decoder.feed(b""), "");
        assert_eq!(decoder.feed("héllo".as_bytes()), "héllo");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn decoder_reconstructs_characters_split_across_chunks() {
        // 2-byte character split 1+1.
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"h\xc3"), "h");
        assert_eq!(decoder.feed(b"\xa9llo"), "éllo");
        assert_eq!(decoder.feed(b"!"), "!");
        assert_eq!(decoder.finish(), "");

        // 3-byte character split 1+2 and 2+1.
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"\xe4"), "");
        assert_eq!(decoder.feed(b"\xbd\xa0"), "你");
        assert_eq!(decoder.feed(b"ok"), "ok");
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"\xe4\xbd"), "");
        assert_eq!(decoder.feed(b"\xa0"), "你");

        // 4-byte character split 1+1+2.
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"\xf0"), "");
        assert_eq!(decoder.feed(b"\x9d"), "");
        assert_eq!(decoder.feed(b"\x84\x9e"), "𝄞");

        // Multiple characters, only the last one split.
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed("héllo wörld ".as_bytes()), "héllo wörld ");
        assert_eq!(decoder.feed(b"\xe4"), "");
        assert_eq!(decoder.feed(b"\xbd"), "");
        assert_eq!(decoder.feed(b"\xa0!"), "你!");
    }

    #[test]
    fn decoder_replaces_genuinely_invalid_bytes() {
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"\xff"), "\u{FFFD}");
        assert_eq!(decoder.feed(b"a\xff\xffb"), "a\u{FFFD}\u{FFFD}b");
        assert_eq!(decoder.feed(b"ok"), "ok");
        // A complete-looking but invalid sequence followed by more text keeps
        // decoding instead of dropping the remainder.
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"\xc3\x28x"), "\u{FFFD}(x");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn decoder_flushes_truncated_tail_lossily() {
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.feed(b"tail \xc3"), "tail ");
        assert_eq!(decoder.finish(), "\u{FFFD}");
        assert_eq!(decoder.finish(), "");
    }

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

    #[cfg(not(windows))]
    #[test]
    fn timeout_applies_when_streams_close_but_process_keeps_running() {
        // The child closes both stdout and stderr immediately (so the stream
        // channel reports Closed) but stays alive for 30s. run_capture_timeout
        // must still honor the deadline instead of blocking on wait() until
        // the process exits on its own.
        let (program, args) = silent_linger_command();
        let result = with_deadline(std::time::Duration::from_secs(15), move || {
            run_capture_timeout(
                &program,
                &args,
                Path::new("."),
                std::time::Duration::from_millis(150),
            )
        })
        .expect("run_capture_timeout hung: streams closed but the process kept running")
        .expect("run_capture_timeout failed");
        assert_eq!(result.exit_code, None);
        assert!(!result.is_success());
        assert!(result.stderr.contains("timed out"));
    }

    #[cfg(not(windows))]
    #[test]
    fn streams_close_early_while_process_keeps_running() {
        // Premise for the "streams closed but process still running" tests:
        // the child must actually close both streams while staying alive, so
        // the stream channel reports Closed before the process exits.
        let (program, args) = silent_linger_command();
        let mut proc = spawn_streaming(&program, &args, Path::new(".")).unwrap();
        assert_eq!(
            proc.next_chunk_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            StreamWait::Closed,
            "test premise broken: the child did not close its streams while running"
        );
        assert!(
            proc.try_wait().unwrap().is_none(),
            "test premise broken: the child should still be running"
        );
        proc.kill().unwrap();
        let output = proc.wait().unwrap();
        assert_ne!(output.exit_code, Some(0));
    }

    #[test]
    fn try_wait_caches_exit_status_for_later_wait() {
        // try_wait() reaps the child on Unix the first time it observes the
        // exit; the status must be cached so a later wait() reuses it instead
        // of failing with "no child processes". This is what lets the
        // deadline-bounded loops call try_wait() freely and still finish with
        // wait().
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
        assert!(
            proc.try_wait().unwrap().is_none(),
            "child should still be running right after spawn"
        );
        proc.kill().unwrap();
        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = proc.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "killed child never exited"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_ne!(status.code(), Some(0));
        // Cached: a second poll returns the same status.
        assert_eq!(proc.try_wait().unwrap(), Some(status));
        // wait() reuses the cached status and still returns the full output.
        let output = proc.wait().unwrap();
        assert_eq!(output.exit_code, status.code());
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

    /// Runs `f` on a helper thread and returns `None` if it does not finish
    /// within `deadline`, so a regression that blocks forever fails the test
    /// instead of hanging the whole suite.
    fn with_deadline<T: Send + 'static>(
        deadline: std::time::Duration,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(f());
        });
        receiver.recv_timeout(deadline).ok()
    }

    /// A unique temp-file path used to record a grandchild's pid.
    fn pid_marker_path(tag: &str) -> std::path::PathBuf {
        let unique = format!(
            "nextide-pid-{tag}-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        std::env::temp_dir().join(unique)
    }

    /// Blocks until `path` contains a parseable pid, up to `deadline`.
    #[allow(dead_code)]
    fn read_pid_marker(path: &std::path::Path, deadline: std::time::Duration) -> Option<u32> {
        let started = std::time::Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    return Some(pid);
                }
            }
            if started.elapsed() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Bounded poll until `pid` no longer reports as running.
    fn wait_until_gone(pid: u32, deadline: std::time::Duration) {
        let started = std::time::Instant::now();
        while pid_is_running(pid) && started.elapsed() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn wait_returns_when_descendant_holds_streams() {
        // The direct child exits immediately, but a grandchild inherits the
        // stdout/stderr pipes and stays alive for 30s. wait() must reap the
        // tree instead of blocking until the grandchild closes the pipes.
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec![
                    "/C".to_string(),
                    "start /b powershell -NoProfile -Command Start-Sleep 30".to_string(),
                ],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "sleep 30 &".to_string()],
            )
        };
        let mut proc = spawn_streaming(&program, &args, Path::new(".")).unwrap();
        let output = with_deadline(std::time::Duration::from_secs(15), move || proc.wait())
            .expect("wait() hung: a descendant kept the stdout/stderr pipes open")
            .expect("wait() failed");
        assert_eq!(output.exit_code, Some(0));
        assert!(
            output.stdout.is_empty(),
            "unexpected stdout: {:?}",
            output.stdout
        );
        assert!(
            output.stderr.is_empty(),
            "unexpected stderr: {:?}",
            output.stderr
        );
    }

    #[test]
    fn kill_and_drop_reap_the_process_tree() {
        // Spawns a command whose direct child stays alive for 30s while a
        // grandchild holding the pipes runs in the background; on Unix the
        // grandchild's pid is recorded in `marker`.
        let tree_args = |marker: &std::path::Path| -> (String, Vec<String>) {
            if cfg!(windows) {
                (
                    "cmd.exe".to_string(),
                    vec![
                        "/C".to_string(),
                        "start /b powershell -NoProfile -Command Start-Sleep 30 & ping -n 30 127.0.0.1 > nul".to_string(),
                    ],
                )
            } else {
                (
                    "sh".to_string(),
                    vec![
                        "-c".to_string(),
                        format!("sleep 30 & echo $! > '{}'; sleep 30", marker.display()),
                    ],
                )
            }
        };

        // kill() must terminate the direct child and every descendant.
        let marker = pid_marker_path("kill");
        let (program, args) = tree_args(&marker);
        let mut proc = spawn_streaming(&program, &args, Path::new(".")).unwrap();
        let direct_pid = proc.id().unwrap();
        #[cfg(not(windows))]
        let grandchild = read_pid_marker(&marker, std::time::Duration::from_secs(10))
            .expect("grandchild pid marker was never written");
        proc.kill().unwrap();
        let output = with_deadline(std::time::Duration::from_secs(15), move || proc.wait())
            .expect("wait() hung after kill(): the process tree was not reaped")
            .expect("wait() failed");
        assert_ne!(output.exit_code, Some(0));
        wait_until_gone(direct_pid, std::time::Duration::from_secs(10));
        assert!(!pid_is_running(direct_pid));
        #[cfg(not(windows))]
        {
            wait_until_gone(grandchild, std::time::Duration::from_secs(10));
            assert!(!pid_is_running(grandchild));
        }
        let _ = std::fs::remove_file(&marker);

        // Dropping the handle must reap the tree the same way.
        let marker = pid_marker_path("drop");
        let (program, args) = tree_args(&marker);
        let proc = spawn_streaming(&program, &args, Path::new(".")).unwrap();
        let direct_pid = proc.id().unwrap();
        #[cfg(not(windows))]
        let grandchild = read_pid_marker(&marker, std::time::Duration::from_secs(10))
            .expect("grandchild pid marker was never written");
        let dropped = with_deadline(std::time::Duration::from_secs(15), move || drop(proc));
        assert!(
            dropped.is_some(),
            "drop() hung: the process tree was not reaped"
        );
        wait_until_gone(direct_pid, std::time::Duration::from_secs(10));
        assert!(!pid_is_running(direct_pid));
        #[cfg(not(windows))]
        {
            wait_until_gone(grandchild, std::time::Duration::from_secs(10));
            assert!(!pid_is_running(grandchild));
        }
        let _ = std::fs::remove_file(&marker);
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

    /// A command whose child closes both stdout and stderr immediately but
    /// keeps running for 30s: the stream channel reports [`StreamWait::Closed`]
    /// while the process is still alive. `exec 1>&- 2>&-` closes the shell's
    /// own fds, so the pipe write ends are gone even though the child keeps
    /// running (`sleep` inherits the closed descriptors).
    ///
    /// Windows cannot reproduce this with stock tools: closing the standard
    /// handles from inside a process (PowerShell/.NET) leaves duplicate
    /// references to the pipe write ends behind, so the streams never report
    /// Closed while the child lives.
    #[cfg(not(windows))]
    fn silent_linger_command() -> (String, Vec<String>) {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "exec 1>&- 2>&-; sleep 30".to_string()],
        )
    }
}
