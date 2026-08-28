use std::io;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
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
        assert!(result.stdout.contains("hello"));
    }
}
