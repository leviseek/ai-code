use nextide_kernel::{ApplicationContext, EditTransaction, EditorKernel, TextEdit};

fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match command.as_str() {
        "smoke" => smoke(),
        "bootstrap" => bootstrap(),
        _ => {
            println!("NextIDE CLI\n\nCommands:\n  smoke      run the editor-kernel smoke flow\n  bootstrap  initialize core capabilities");
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
