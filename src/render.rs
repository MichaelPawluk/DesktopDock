//! Drawing. Direct2D (software mode, so no GPU device sits in memory) draws into a 32-bit
//! bitmap, and UpdateLayeredWindow puts it on screen with per-pixel transparency. Both are
//! documented APIs that have been stable for well over a decade.
//! Transparent pixels are automatically click-through.

use crate::icons::Pixels;
use std::ffi::c_void;
use std::path::Path;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_PROPERTIES, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP,
    HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{Error, Result};

const PIXEL_FORMAT: D2D1_PIXEL_FORMAT = D2D1_PIXEL_FORMAT {
    format: DXGI_FORMAT_B8G8R8A8_UNORM,
    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
};

/// A 32-bit top-down DIB selected into a memory DC.
pub struct Surface {
    hdc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub width: i32,
    pub height: i32,
}

impl Surface {
    fn new(width: i32, height: i32) -> Result<Self> {
        unsafe {
            let screen = GetDC(None);
            let hdc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(hdc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) => bitmap,
                Err(e) => {
                    let _ = DeleteDC(hdc);
                    return Err(e);
                }
            };
            let old = SelectObject(hdc, bitmap.into());
            Ok(Self { hdc, bitmap, old, bits: bits as *mut u8, width, height })
        }
    }

    pub fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.width * self.height * 4) as usize) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.hdc);
        }
    }
}

pub struct Renderer {
    pub rt: ID2D1DCRenderTarget,
    pub dwrite: IDWriteFactory,
    _factory: ID2D1Factory,
    surface: Option<Surface>,
    /// The dragged icon's own small buffer (see `begin_drag_image`).
    drag_surface: Option<Surface>,
    /// An open group's pop-up (see `begin_popup`).
    popup_surface: Option<Surface>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: PIXEL_FORMAT,
                dpiX: 96.0, // we scale ourselves, so 1 Direct2D unit = 1 physical pixel
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dwrite = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self { rt, dwrite, _factory: factory, surface: None, drag_surface: None, popup_surface: None })
        }
    }

    pub fn bitmap(&self, pixels: &Pixels) -> Result<ID2D1Bitmap> {
        unsafe {
            self.rt.CreateBitmap(
                D2D_SIZE_U { width: pixels.width, height: pixels.height },
                Some(pixels.data.as_ptr() as *const c_void),
                pixels.width * 4,
                &D2D1_BITMAP_PROPERTIES { pixelFormat: PIXEL_FORMAT, dpiX: 96.0, dpiY: 96.0 },
            )
        }
    }

    /// Starts a frame of the given size, cleared to fully transparent.
    pub fn begin(&mut self, width: i32, height: i32) -> Result<()> {
        let fits = self.surface.as_ref().is_some_and(|s| s.width == width && s.height == height);
        if !fits {
            self.surface = None;
            self.surface = Some(Surface::new(width, height)?);
        }
        let surface = self.surface.as_ref().ok_or_else(Error::empty)?;
        unsafe {
            self.rt.BindDC(surface.hdc, &RECT { left: 0, top: 0, right: width, bottom: height })?;
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
        Ok(())
    }

    /// For tests: the drawing buffers that exist now.
    pub fn describe(&self) -> String {
        let size = |surface: &Option<Surface>| surface.as_ref().map_or("none".to_string(), |s| format!("{}x{}", s.width, s.height));
        format!("surface={} popup_surface={} drag_surface={}", size(&self.surface), size(&self.popup_surface), size(&self.drag_surface))
    }

    /// Frees the drawing buffer (about 4 MB for a full-width dock). Windows keeps its own copy
    /// of what's on screen, so this is safe whenever the dock is hidden; the next frame
    /// allocates a new one.
    pub fn release(&mut self) {
        self.surface = None;
    }

    /// Direct2D frees released bitmaps only when a frame ends, so while the dock is hidden (no
    /// frames) replaced icons would pile up. This ends an empty 1x1 frame to let it tidy up,
    /// then frees that tiny buffer again. Only for when the dock is hidden.
    pub fn settle(&mut self) {
        if self.begin(1, 1).is_ok() {
            let _ = self.end();
        }
        self.release();
    }

    /// Starts drawing the image that follows the pointer while an icon is dragged. It has its own
    /// small buffer but the same render target, because Direct2D bitmaps (the icons) only draw
    /// on the target that made them.
    pub fn begin_drag_image(&mut self, side: i32) -> Result<()> {
        if !self.drag_surface.as_ref().is_some_and(|s| s.width == side && s.height == side) {
            self.drag_surface = None;
            self.drag_surface = Some(Surface::new(side, side)?);
        }
        let surface = self.drag_surface.as_ref().ok_or_else(Error::empty)?;
        unsafe {
            self.rt.BindDC(surface.hdc, &RECT { left: 0, top: 0, right: side, bottom: side })?;
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
        Ok(())
    }

    /// Shows (or moves) the drag image at (x, y) in screen pixels.
    pub fn present_drag_image(&self, hwnd: HWND, x: i32, y: i32) -> Result<()> {
        present_surface(self.drag_surface.as_ref().ok_or_else(Error::empty)?, hwnd, x, y)
    }

    pub fn release_drag_image(&mut self) {
        self.drag_surface = None;
    }

    /// Starts drawing an open group's pop-up: its own buffer, the same render target (the
    /// icons are the dock's). Freed when the pop-up closes.
    pub fn begin_popup(&mut self, width: i32, height: i32) -> Result<()> {
        if !self.popup_surface.as_ref().is_some_and(|s| s.width == width && s.height == height) {
            self.popup_surface = None;
            self.popup_surface = Some(Surface::new(width, height)?);
        }
        let surface = self.popup_surface.as_ref().ok_or_else(Error::empty)?;
        unsafe {
            self.rt.BindDC(surface.hdc, &RECT { left: 0, top: 0, right: width, bottom: height })?;
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
        Ok(())
    }

    /// Shows the pop-up at (x, y) in screen pixels.
    pub fn present_popup(&self, hwnd: HWND, x: i32, y: i32) -> Result<()> {
        present_surface(self.popup_surface.as_ref().ok_or_else(Error::empty)?, hwnd, x, y)
    }

    /// The same, `alpha` (0..1) see-through: a closing pop-up fading out.
    pub fn present_popup_faded(&self, hwnd: HWND, x: i32, y: i32, alpha: f32) -> Result<()> {
        present_surface_alpha(self.popup_surface.as_ref().ok_or_else(Error::empty)?, hwnd, x, y, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
    }

    pub fn release_popup(&mut self) {
        self.popup_surface = None;
    }

    /// Test helper: the pop-up's last frame as a BMP (like `save_bmp`).
    pub fn save_popup_bmp(&self, path: &Path) -> std::io::Result<()> {
        save_surface_bmp(self.popup_surface.as_ref().ok_or(std::io::ErrorKind::NotFound)?, path)
    }

    pub fn end(&mut self) -> Result<()> {
        unsafe { self.rt.EndDraw(None, None) }
    }

    /// Puts the last frame on screen at (x, y) in physical screen pixels.
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) -> Result<()> {
        present_surface(self.surface.as_ref().ok_or_else(Error::empty)?, hwnd, x, y)
    }

    /// Test helper: writes the last frame, composited over a desktop-like background, as a
    /// 24-bit BMP.
    pub fn save_bmp(&self, path: &Path) -> std::io::Result<()> {
        save_surface_bmp(self.surface.as_ref().ok_or(std::io::ErrorKind::NotFound)?, path)
    }

    /// Test helper: the same for the drag image.
    pub fn save_drag_bmp(&self, path: &Path) -> std::io::Result<()> {
        save_surface_bmp(self.drag_surface.as_ref().ok_or(std::io::ErrorKind::NotFound)?, path)
    }
}

fn save_surface_bmp(surface: &Surface, path: &Path) -> std::io::Result<()> {
    let (w, h) = (surface.width as usize, surface.height as usize);
    let row_bytes = (w * 3 + 3) & !3;
    let mut out = Vec::with_capacity(54 + row_bytes * h);
    let file_size = (54 + row_bytes * h) as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes()); // bottom-up
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    let src = surface.pixels();
    for y in (0..h).rev() {
        let start = out.len();
        for x in 0..w {
            let p = &src[(y * w + x) * 4..(y * w + x) * 4 + 4];
            // Background: a muted blue-grey gradient, like a typical wallpaper.
            let t = y as f32 / h.max(1) as f32;
            let bg = [(70.0 + 40.0 * t) as f32, (60.0 + 30.0 * t) as f32, (45.0 + 20.0 * t) as f32];
            let a = p[3] as f32 / 255.0;
            for c in 0..3 {
                out.push((p[c] as f32 + bg[c] * (1.0 - a)).round().min(255.0) as u8);
            }
        }
        out.resize(start + row_bytes, 0);
    }
    std::fs::write(path, out)
}

/// Puts a finished buffer on screen as a layered window's picture, at (x, y) in screen pixels.
fn present_surface(surface: &Surface, hwnd: HWND, x: i32, y: i32) -> Result<()> {
    present_surface_alpha(surface, hwnd, x, y, 255)
}

fn present_surface_alpha(surface: &Surface, hwnd: HWND, x: i32, y: i32, alpha: u8) -> Result<()> {
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        let screen = GetDC(None);
        let result = UpdateLayeredWindow(
            hwnd,
            Some(screen),
            Some(&POINT { x, y }),
            Some(&SIZE { cx: surface.width, cy: surface.height }),
            Some(surface.hdc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(None, screen);
        result
    }
}
