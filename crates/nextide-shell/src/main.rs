use nextide_shell::DesktopShell;

fn main() {
    let mut shell = DesktopShell::new("NextIDE");
    let window = shell
        .create_main_window()
        .expect("main window should be created");
    println!("{}; window={}", shell.start(), window.id);
}
