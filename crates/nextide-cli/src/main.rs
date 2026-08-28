use nextide_kernel::{
    ApplicationContext, AsyncTaskStatus, EditTransaction, EditorKernel, EventBus, TaskManager,
    TaskSpec, TextEdit,
};
use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match command.as_str() {
        "smoke" => smoke(),
        "bootstrap" => bootstrap(),
        "task-smoke" => task_smoke(),
        _ => {
            println!(
                "NextIDE CLI\n\nCommands:\n  smoke       run the editor-kernel smoke flow\n  bootstrap   initialize core capabilities\n  task-smoke  run a background task through TaskManager::spawn_async"
            );
        }
    }
}

fn bootstrap() {
    let context = ApplicationContext::bootstrap("");
    assert!(context.capabilities.contains("workspace.read"));
    assert!(context.capabilities.contains("workspace.write"));
    assert!(context.capabilities.contains("terminal.execute"));
    println!("capabilities: {}", context.capabilities.len());
    println!("status: ready");
}

fn smoke() {
    let mut kernel = EditorKernel::new("fn main() {}\n");
    let initial_revision = kernel.document().revision();
    let edit = TextEdit::replace(3, 7, "start");
    let applied = kernel
        .apply(EditTransaction::new(
            "rename-entrypoint",
            initial_revision,
            vec![edit],
        ))
        .expect("smoke edit should apply");

    println!("applied: {}", applied.label);
    println!("revision: {}", kernel.document().revision());
    println!("text: {}", kernel.document().text().trim_end());

    kernel.undo().expect("smoke undo should work");
    assert_eq!(kernel.document().text(), "fn main() {}\n");
    kernel.redo().expect("smoke redo should work");
    assert_eq!(kernel.document().text(), "fn start() {}\n");
    println!("status: ok");
}

/// Runs a background task through the kernel's [`TaskManager`]: registers a
/// cross-platform echo command, spawns it with
/// [`TaskManager::spawn_async`], polls [`AsyncTaskHandle::wait_timeout`] until
/// it completes within a finite deadline, and prints the task's status and
/// output. Panics if the task fails, times out, is cancelled, or exceeds the
/// deadline.
fn task_smoke() {
    // Cross-platform trivial command: `cmd.exe /C echo` on Windows,
    // `sh -c printf` everywhere else (mirrors the kernel's own tests).
    let (program, args) = if cfg!(windows) {
        (
            "cmd.exe",
            vec!["/C".into(), "echo nextide-task-smoke".into()],
        )
    } else {
        ("sh", vec!["-c".into(), "printf nextide-task-smoke".into()])
    };
    let mut manager = TaskManager::default();
    let mut events = EventBus::default();
    manager
        .add(TaskSpec {
            id: "task-smoke".into(),
            program: program.into(),
            args,
        })
        .expect("task-smoke spec should register");
    let handle = manager
        .spawn_async("task-smoke", Path::new("."), &mut events)
        .expect("task-smoke process should spawn");

    // Poll with a short slice so intermediate status and output are
    // observable, and enforce an overall deadline so the smoke can never hang.
    let deadline = Instant::now() + Duration::from_secs(10);
    let snapshot = loop {
        match handle.wait_timeout(Duration::from_millis(200)) {
            Some(snapshot) => break snapshot,
            None => {
                println!("status: {:?}", handle.state());
                let output = handle.drain_output();
                if !output.is_empty() {
                    println!("output: {}", output.trim_end());
                }
                if Instant::now() >= deadline {
                    handle.cancel();
                    panic!("task-smoke did not finish within the 10s deadline");
                }
            }
        }
    };

    println!("status: {:?}", snapshot.status);
    let output = snapshot.output.trim_end();
    println!("output: {output}");
    match snapshot.status {
        AsyncTaskStatus::Succeeded { .. } => println!("status: ok"),
        other => panic!("task-smoke failed: {other:?}"),
    }
}
