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

#[cfg(test)]
mod tests {
    use super::*;
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
