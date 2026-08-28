use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

pub trait RenderSurface {
    fn resize(&mut self, size: SurfaceSize) -> io::Result<()>;
    fn present(&mut self) -> io::Result<()>;
}

#[derive(Debug)]
pub struct HeadlessSurface {
    pub size: SurfaceSize,
    frames: u64,
}

impl HeadlessSurface {
    pub fn new(size: SurfaceSize) -> Self {
        Self { size, frames: 0 }
    }
    pub fn frames_presented(&self) -> u64 {
        self.frames
    }
}

impl RenderSurface for HeadlessSurface {
    fn resize(&mut self, size: SurfaceSize) -> io::Result<()> {
        self.size = size;
        Ok(())
    }
    fn present(&mut self) -> io::Result<()> {
        self.frames += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_surface_resizes_and_presents() {
        let mut surface = HeadlessSurface::new(SurfaceSize {
            width: 800,
            height: 600,
        });
        surface
            .resize(SurfaceSize {
                width: 1024,
                height: 768,
            })
            .unwrap();
        surface.present().unwrap();
        assert_eq!(surface.size.width, 1024);
        assert_eq!(surface.frames_presented(), 1);
    }
}
