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
    fn open_terminal(&self, _working_directory: &Path) -> io::Result<Box<dyn TerminalSession>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ConPTY adapter pending",
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
            "Windows process adapter pending",
        ))
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
