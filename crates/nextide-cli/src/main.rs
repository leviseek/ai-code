use nextide_kernel::{EditTransaction, EditorKernel, TextEdit};

fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match command.as_str() {
        "smoke" => smoke(),
        _ => {
            println!("NextIDE CLI\n\nCommands:\n  smoke    run the editor-kernel smoke flow");
        }
    }
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
