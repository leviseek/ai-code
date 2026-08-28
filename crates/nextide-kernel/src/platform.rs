use std::io;
use std::path::Path;

pub trait PlatformServices {
    fn open_terminal(&self, working_directory: &Path) -> io::Result<Box<dyn TerminalSession>>;
    fn spawn(
        &self,
        program: &str,
        args: &[String],
        working_directory: &Path,
    ) -> io::Result<ProcessHandle>;

    fn terminate(&self, _process: &ProcessHandle) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process termination adapter pending",
        ))
    }

    fn is_running(&self, _process: &ProcessHandle) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process status adapter pending",
        ))
    }
}

pub trait TerminalSession: Send {
    fn write_input(&mut self, input: &[u8]) -> io::Result<()>;
    fn resize(&mut self, columns: u16, rows: u16) -> io::Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessHandle {
    pub id: u32,
}

#[cfg(windows)]
pub struct WindowsPlatform;

#[cfg(windows)]
impl PlatformServices for WindowsPlatform {
    fn terminate(&self, process: &ProcessHandle) -> io::Result<()> {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &process.id.to_string(), "/T", "/F"])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("taskkill failed"))
        }
    }

    fn is_running(&self, process: &ProcessHandle) -> io::Result<bool> {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", process.id)])
            .output()?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(&process.id.to_string()))
    }

    fn open_terminal(&self, _working_directory: &Path) -> io::Result<Box<dyn TerminalSession>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ConPTY adapter pending",
        ))
    }

    fn spawn(
        &self,
        program: &str,
        args: &[String],
        working_directory: &Path,
    ) -> io::Result<ProcessHandle> {
        let child = std::process::Command::new(program)
            .args(args)
            .current_dir(working_directory)
            .spawn()?;
        Ok(ProcessHandle { id: child.id() })
    }
}

#[cfg(windows)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawns_process_in_requested_directory() {
        let platform = WindowsPlatform;
        let handle = platform
            .spawn("cmd.exe", &["/C".into(), "exit".into()], Path::new("."))
            .unwrap();
        assert!(handle.id > 0);
    }
}

#[cfg(not(windows))]
pub struct WindowsPlatform;

#[cfg(not(windows))]
impl PlatformServices for WindowsPlatform {
    fn open_terminal(&self, _working_directory: &Path) -> io::Result<Box<dyn TerminalSession>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows platform unavailable on this host",
        ))
    }

    fn spawn(
        &self,
        _program: &str,
        _args: &[String],
        _working_directory: &Path,
    ) -> io::Result<ProcessHandle> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows platform unavailable on this host",
        ))
    }
}
