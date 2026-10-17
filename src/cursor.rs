//! Software cursor image for the TTY backend (the nested backend uses the host cursor).

use std::{collections::HashMap, fs};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::CursorIcon,
    utils::{Physical, Point, Transform},
};

pub struct CursorImage {
    pub buffer: MemoryRenderBuffer,
    pub hotspot: Point<i32, Physical>,
}

/// Named cursors (cursor-shape-v1 and plain `Named` requests) from the XCursor theme
/// (`$XCURSOR_THEME`, `$XCURSOR_SIZE`), loaded lazily. Shapes the theme lacks fall back to the
/// default arrow, which in turn falls back to a built-in drawing.
pub struct CursorTheme {
    theme: xcursor::CursorTheme,
    size: u32,
    cache: HashMap<CursorIcon, CursorImage>,
    fallback: CursorImage,
}

impl CursorTheme {
    pub fn load() -> Self {
        let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        let size: u32 = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
        let theme = xcursor::CursorTheme::load(&name);
        let fallback = CursorImage::from_theme(&theme, "default", size).unwrap_or_else(|| {
            tracing::warn!("no xcursor theme {name:?} found, using a built-in cursor");
            CursorImage::fallback()
        });
        Self { theme, size, cache: HashMap::new(), fallback }
    }

    pub fn get(&mut self, icon: CursorIcon) -> &CursorImage {
        if !self.cache.contains_key(&icon) {
            let found = std::iter::once(icon.name())
                .chain(icon.alt_names().iter().copied())
                .find_map(|n| CursorImage::from_theme(&self.theme, n, self.size));
            match found {
                Some(image) => {
                    self.cache.insert(icon, image);
                }
                None => return &self.fallback,
            }
        }
        &self.cache[&icon]
    }
}

impl CursorImage {
    fn from_theme(theme: &xcursor::CursorTheme, name: &str, size: u32) -> Option<Self> {
        let path = theme.load_icon(name)?;
        let images = xcursor::parser::parse_xcursor(&fs::read(path).ok()?)?;
        let image = images.iter().min_by_key(|i| (size as i32 - i.size as i32).abs())?;
        // xcursor stores RGBA; Argb8888 in memory (little endian) is B, G, R, A.
        let mut argb = Vec::with_capacity(image.pixels_rgba.len());
        for px in image.pixels_rgba.chunks_exact(4) {
            argb.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        Some(Self::make(&argb, image.width as i32, image.height as i32, image.xhot as i32, image.yhot as i32))
    }

    fn fallback() -> Self {
        const N: i32 = 16;
        let mut px = vec![0u8; (N * N * 4) as usize];
        for y in 0..N {
            for x in 0..=(y / 2).min(N - 1) {
                let edge = x == 0 || x == y / 2 || y == N - 1;
                let c = if edge { 0u8 } else { 255u8 };
                let i = ((y * N + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[c, c, c, 255]);
            }
        }
        Self::make(&px, N, N, 0, 0)
    }

    fn make(argb: &[u8], w: i32, h: i32, xhot: i32, yhot: i32) -> Self {
        Self {
            buffer: MemoryRenderBuffer::from_slice(argb, Fourcc::Argb8888, (w, h), 1, Transform::Normal, None),
            hotspot: (xhot, yhot).into(),
        }
    }
}
