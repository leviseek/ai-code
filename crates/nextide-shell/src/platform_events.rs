use crate::{KeyChord, Point, UiInput};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformEvent {
    MouseMove {
        x: u32,
        y: u32,
    },
    MouseClick {
        x: u32,
        y: u32,
    },
    KeyDown(KeyChord),
    Resize {
        width: u32,
        height: u32,
    },
    CloseRequested,
    FileClicked {
        path: std::path::PathBuf,
        directory: bool,
    },
}

pub fn to_ui_input(event: PlatformEvent) -> Option<UiInput> {
    match event {
        PlatformEvent::KeyDown(chord) => Some(UiInput::Key(chord)),
        PlatformEvent::Resize { width, height } => Some(UiInput::Resize { width, height }),
        PlatformEvent::CloseRequested => Some(UiInput::Close),
        PlatformEvent::MouseMove { .. } => None,
        PlatformEvent::FileClicked { path, directory } => Some(if directory {
            UiInput::ClickDirectory(path)
        } else {
            UiInput::ClickFile(path)
        }),
    }
}

pub fn point(event: PlatformEvent) -> Option<Point> {
    match event {
        PlatformEvent::MouseMove { x, y } => Some(Point { x, y }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_platform_lifecycle_events() {
        assert_eq!(
            to_ui_input(PlatformEvent::Resize {
                width: 1,
                height: 2
            }),
            Some(UiInput::Resize {
                width: 1,
                height: 2
            })
        );
        assert_eq!(
            to_ui_input(PlatformEvent::CloseRequested),
            Some(UiInput::Close)
        );
        assert_eq!(
            point(PlatformEvent::MouseMove { x: 3, y: 4 }),
            Some(Point { x: 3, y: 4 })
        );
    }
}
