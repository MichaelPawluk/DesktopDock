//! Icons, loaded at exactly the pixel size they're drawn so they stay sharp at any scaling.
//!
//! Sources, tried in order until one works (fail soft):
//! - an image file (PNG, ICO, JPG, …), or an icon inside a file (`file,-id` / `file,index`);
//! - Windows' own icon for the target (the same lookup Explorer uses);
//! - for the Recycle Bin, Windows' stock empty/full icons.
//!
//! Loading happens on a worker thread (Microsoft: icon extraction must not run on the UI
//! thread). Results are cached in `cache\icons\` as raw pre-scaled pixels, so the next start
//! reads small files instead of decoding 2048 px images or asking the shell again.

use crate::config::{ItemConfig, Kind, RECYCLE_BIN};
use crate::home::Home;
use crate::log_warn;
use crate::places;
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::{GENERIC_READ, HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, ReleaseDC,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::{
    ExtractIconExW, IShellItemImageFactory, SHCreateItemFromParsingName, SHDefExtractIconW, SHGSI_ICONLOCATION,
    SHGetStockIconInfo, SHSTOCKICONINFO, SIID_RECYCLER, SIID_RECYCLERFULL, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, PostMessageW};
use windows::core::{Error, HSTRING, Interface, Result};

/// Bump when the cache format or scaling changes, so old cache files are ignored.
/// 2: icons inside files are see-through where their mask says (they were opaque black there).
const CACHE_VERSION: u32 = 2;
const IMAGE_EXTENSIONS: &[&str] = &["png", "ico", "jpg", "jpeg", "bmp", "gif", "tif", "tiff"];

/// Premultiplied BGRA pixels, top-down, stride = width * 4.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// An icon's main colour, for the hover glow: the most common vivid hue among its solid pixels,
/// brightened so it glows. None when the icon has hardly any colour (greys, black and white).
pub fn main_colour(pixels: &Pixels) -> Option<[f32; 3]> {
    const BINS: usize = 24;
    let mut weight = [0f32; BINS];
    let mut sum = [[0f32; 3]; BINS];
    let mut solid = 0f32;
    for p in pixels.data.chunks_exact(4) {
        let a = p[3] as f32 / 255.0;
        if a < 0.5 {
            continue;
        }
        solid += 1.0;
        // Premultiplied BGRA.
        let (r, g, b) = (p[2] as f32 / 255.0 / a, p[1] as f32 / 255.0 / a, p[0] as f32 / 255.0 / a);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
        if saturation < 0.25 || max < 0.2 {
            continue;
        }
        let d = max - min;
        let hue = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } / 6.0;
        let bin = ((hue * BINS as f32) as usize).min(BINS - 1);
        let w = saturation * max;
        weight[bin] += w;
        for (total, c) in sum[bin].iter_mut().zip([r, g, b]) {
            *total += c * w;
        }
    }
    // The strongest hue, with its neighbours (a hue split across two bins counts once).
    let around = |bin: usize| [(bin + BINS - 1) % BINS, bin, (bin + 1) % BINS];
    let best = (0..BINS).max_by(|&a, &b| {
        let w = |bin: usize| around(bin).iter().map(|&k| weight[k]).sum::<f32>();
        w(a).total_cmp(&w(b))
    })?;
    let w: f32 = around(best).iter().map(|&k| weight[k]).sum();
    if solid == 0.0 || w < solid * 0.05 {
        return None; // hardly any colour
    }
    let mut colour = [0f32; 3];
    for k in around(best) {
        for (c, total) in colour.iter_mut().zip(sum[k]) {
            *c += total / w;
        }
    }
    Some(glow_bright(colour))
}

/// A colour made bright enough to glow (its brightest channel at least 85%).
pub fn glow_bright(colour: [f32; 3]) -> [f32; 3] {
    let max = colour[0].max(colour[1]).max(colour[2]);
    if max <= 0.0 {
        return [0.85, 0.85, 0.85];
    }
    let scale = max.max(0.85) / max;
    colour.map(|c| (c * scale).min(1.0))
}

/// What an icon is made from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconSource {
    Image(String),
    /// An icon inside a file. Negative = resource id, otherwise index (Windows' convention).
    Resource(String, i32),
    /// Windows' own icon for a target: app, file, folder or shell location.
    Shell(String),
    RecycleBin { full: bool },
}

/// Splits `C:\x\imageres.dll,-109` into the file and the icon number; plain paths have none.
pub fn parse_icon_ref(icon: &str) -> (String, Option<i32>) {
    let icon = icon.trim();
    if let Some((file, number)) = icon.rsplit_once(',') {
        if let Ok(n) = number.trim().parse::<i32>() {
            return (file.trim().to_string(), Some(n));
        }
    }
    (icon.to_string(), None)
}

/// The sources to try for an item, best first. `item` must already have its paths resolved.
pub fn sources_for(item: &ItemConfig, bin_full: bool) -> Vec<IconSource> {
    let mut sources = Vec::new();
    if !item.icon.trim().is_empty() {
        let (file, number) = parse_icon_ref(&item.icon);
        let is_image = Path::new(&file)
            .extension()
            .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)));
        match number {
            Some(n) if !is_image => sources.push(IconSource::Resource(file, n)),
            _ if is_image => sources.push(IconSource::Image(file)),
            _ => sources.push(IconSource::Resource(file, 0)),
        }
    }
    match item.kind {
        Kind::RecycleBin => {
            sources.push(IconSource::RecycleBin { full: bin_full });
            sources.push(IconSource::Shell(RECYCLE_BIN.into()));
        }
        _ if !item.target.trim().is_empty() => sources.push(IconSource::Shell(item.target.trim().into())),
        _ => {}
    }
    sources
}

/// A stable cache file name for these sources at this size. Includes each source file's size
/// and modified time, so an updated app or edited image gets a fresh icon.
pub fn cache_key(sources: &[IconSource], size: u32) -> String {
    let mut text = format!("v{CACHE_VERSION}|{size}");
    for source in sources {
        text.push_str(&format!("|{source:?}"));
        let file = match source {
            IconSource::Image(f) | IconSource::Resource(f, _) | IconSource::Shell(f) => Some(f),
            IconSource::RecycleBin { .. } => None,
        };
        if let Some(meta) = file.and_then(|f| std::fs::metadata(f).ok()).filter(|m| m.is_file()) {
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            text.push_str(&format!("@{}:{}", meta.len(), modified.map_or(0, |d| d.as_secs())));
        }
    }
    format!("{:016x}", fnv1a(text.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

// ---- Cache files: "DDI1" + width + height + raw premultiplied BGRA --------------------------

fn read_cached(path: &Path, size: u32) -> Option<Pixels> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 12 || &bytes[..4] != b"DDI1" {
        return None;
    }
    let width = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let ok = width == size && height == size && bytes.len() == 12 + (width * height * 4) as usize;
    ok.then(|| Pixels { width, height, data: bytes[12..].to_vec() })
}

fn write_cached(path: &Path, pixels: &Pixels) {
    let mut bytes = Vec::with_capacity(12 + pixels.data.len());
    bytes.extend_from_slice(b"DDI1");
    bytes.extend_from_slice(&pixels.width.to_le_bytes());
    bytes.extend_from_slice(&pixels.height.to_le_bytes());
    bytes.extend_from_slice(&pixels.data);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Deletes cache files that aren't in `keep` (icons no longer on the dock, old sizes).
pub fn prune_cache(dir: &Path, keep: &HashSet<String>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if !keep.contains(&stem) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// How many icons a file (.exe, .dll, .ico) contains; 0 if none or unreadable.
pub fn icon_count(path: &str) -> u32 {
    unsafe { ExtractIconExW(&HSTRING::from(path), -1, None, None, 0) }
}

// ---- Loading (any thread with COM initialised) ---------------------------------------------

pub struct IconLoader {
    wic: IWICImagingFactory,
    cache_dir: Option<PathBuf>,
    /// Safe mode: only use cached icons; never decode images or ask the shell.
    cache_only: bool,
}

impl IconLoader {
    pub fn new(cache_dir: Option<PathBuf>) -> Result<Self> {
        let wic = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        if let Some(dir) = &cache_dir {
            let _ = std::fs::create_dir_all(dir);
        }
        Ok(Self { wic, cache_dir, cache_only: false })
    }

    /// The icon at `size` x `size`: from the cache if possible, otherwise built from the first
    /// source that works (and then cached).
    pub fn load(&self, sources: &[IconSource], size: u32) -> Option<Pixels> {
        let cache_file = self.cache_dir.as_ref().map(|dir| dir.join(format!("{}.bgra", cache_key(sources, size))));
        if let Some(pixels) = cache_file.as_deref().and_then(|f| read_cached(f, size)) {
            return Some(pixels);
        }
        if self.cache_only {
            return None;
        }
        let pixels = sources.iter().find_map(|source| match self.source(source) {
            Ok(image) => self.render(&image, size).ok(),
            Err(_) => None,
        })?;
        if let Some(file) = &cache_file {
            write_cached(file, &pixels);
        }
        Some(pixels)
    }

    fn source(&self, source: &IconSource) -> Result<IWICBitmapSource> {
        match source {
            IconSource::Image(path) => self.from_file(path),
            IconSource::Resource(path, index) => self.from_resource(path, *index),
            IconSource::Shell(target) => self.from_shell(target),
            IconSource::RecycleBin { full } => {
                let mut info = SHSTOCKICONINFO { cbSize: size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
                let id = if *full { SIID_RECYCLERFULL } else { SIID_RECYCLER };
                unsafe { SHGetStockIconInfo(id, SHGSI_ICONLOCATION, &mut info)? };
                let len = info.szPath.iter().position(|&c| c == 0).unwrap_or(info.szPath.len());
                self.from_resource(&String::from_utf16_lossy(&info.szPath[..len]), info.iIcon)
            }
        }
    }

    /// Scales a source to fit a `size` x `size` square (aspect ratio kept, centred).
    fn render(&self, source: &IWICBitmapSource, size: u32) -> Result<Pixels> {
        unsafe {
            let (mut w, mut h) = (0u32, 0u32);
            source.GetSize(&mut w, &mut h)?;
            if w == 0 || h == 0 || size == 0 {
                return Err(Error::empty());
            }
            let fit = size as f32 / w.max(h) as f32;
            let tw = ((w as f32 * fit).round() as u32).clamp(1, size);
            let th = ((h as f32 * fit).round() as u32).clamp(1, size);

            let scaler = self.wic.CreateBitmapScaler()?;
            scaler.Initialize(source, tw, th, WICBitmapInterpolationModeHighQualityCubic)?;
            let mut scaled = vec![0u8; (tw * th * 4) as usize];
            scaler.CopyPixels(std::ptr::null(), tw * 4, &mut scaled)?;

            let mut data = vec![0u8; (size * size * 4) as usize];
            let (ox, oy) = ((size - tw) / 2, (size - th) / 2);
            for row in 0..th {
                let src = (row * tw * 4) as usize;
                let dst = (((oy + row) * size + ox) * 4) as usize;
                data[dst..dst + (tw * 4) as usize].copy_from_slice(&scaled[src..src + (tw * 4) as usize]);
            }
            Ok(Pixels { width: size, height: size, data })
        }
    }

    fn from_file(&self, path: &str) -> Result<IWICBitmapSource> {
        unsafe {
            let decoder =
                self.wic.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
            // .ico files hold several sizes; take the largest.
            let mut best = None;
            let mut best_area = 0u64;
            for index in 0..decoder.GetFrameCount()? {
                let frame = decoder.GetFrame(index)?;
                let (mut w, mut h) = (0u32, 0u32);
                frame.GetSize(&mut w, &mut h)?;
                if (w as u64) * (h as u64) > best_area {
                    best_area = (w as u64) * (h as u64);
                    best = Some(frame);
                }
            }
            let frame = best.ok_or_else(Error::empty)?;
            self.to_pbgra(&frame.cast()?)
        }
    }

    /// An icon inside an .exe/.dll/.ico, at 256 px (Windows picks the best frame it has).
    fn from_resource(&self, path: &str, index: i32) -> Result<IWICBitmapSource> {
        unsafe {
            let mut icon = HICON::default();
            SHDefExtractIconW(&HSTRING::from(path), index, 0, Some(&mut icon), None, 256).ok()?;
            if icon.is_invalid() {
                return Err(Error::empty());
            }
            let pixels = icon_pixels(icon);
            let bitmap = if pixels.is_err() { Some(self.wic.CreateBitmapFromHICON(icon)) } else { None };
            let _ = DestroyIcon(icon);
            match (pixels, bitmap) {
                (Ok(pixels), _) => self
                    .wic
                    .CreateBitmapFromMemory(pixels.width, pixels.height, &GUID_WICPixelFormat32bppPBGRA, pixels.width * 4, &pixels.data)?
                    .cast(),
                (Err(_), Some(bitmap)) => self.to_pbgra(&bitmap?.cast()?),
                (Err(e), None) => Err(e),
            }
        }
    }

    fn from_shell(&self, target: &str) -> Result<IWICBitmapSource> {
        unsafe {
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(target), None)?;
            let hbitmap = factory.GetImage(SIZE { cx: 256, cy: 256 }, SIIGBF_ICONONLY)?;
            let pixels = hbitmap_pixels(hbitmap);
            let _ = DeleteObject(hbitmap.into());
            let pixels = pixels?;
            let bitmap = self.wic.CreateBitmapFromMemory(
                pixels.width,
                pixels.height,
                &GUID_WICPixelFormat32bppPBGRA,
                pixels.width * 4,
                &pixels.data,
            )?;
            bitmap.cast()
        }
    }

    fn to_pbgra(&self, source: &IWICBitmapSource) -> Result<IWICBitmapSource> {
        unsafe {
            let converter = self.wic.CreateFormatConverter()?;
            converter.Initialize(
                source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            converter.cast()
        }
    }
}

/// An icon's pixels, premultiplied. An icon with no alpha (older programs' icons, and Windows'
/// blank ones) is see-through where its mask says, as Windows draws it; WIC's own conversion
/// leaves those parts opaque black. Monochrome icons are left to WIC.
unsafe fn icon_pixels(icon: HICON) -> Result<Pixels> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info)?;
        let pixels = if info.hbmColor.is_invalid() {
            Err(Error::empty())
        } else {
            bitmap_bits(info.hbmColor).and_then(|(width, height, mut data)| {
                if data.chunks_exact(4).all(|p| p[3] == 0) {
                    let (mask_width, mask_height, mask) = bitmap_bits(info.hbmMask)?;
                    if mask_width != width || mask_height < height {
                        return Err(Error::empty());
                    }
                    // A white mask pixel is see-through.
                    for (p, m) in data.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                        if m[0] != 0 {
                            p.copy_from_slice(&[0, 0, 0, 0]);
                        } else {
                            p[3] = 255;
                        }
                    }
                } else {
                    // Icons' alpha is straight.
                    for p in data.chunks_exact_mut(4) {
                        let a = p[3] as u32;
                        for c in &mut p[..3] {
                            *c = ((*c as u32 * a + 127) / 255) as u8;
                        }
                    }
                }
                Ok(Pixels { width: width as u32, height: height as u32, data })
            })
        };
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        pixels
    }
}

/// A bitmap's pixels as 32-bit BGRA, top-down: width, height, bytes.
unsafe fn bitmap_bits(hbitmap: HBITMAP) -> Result<(i32, i32, Vec<u8>)> {
    unsafe {
        let mut info = BITMAP::default();
        let read = GetObjectW(hbitmap.into(), size_of::<BITMAP>() as i32, Some(&mut info as *mut BITMAP as *mut c_void));
        if read == 0 || info.bmWidth <= 0 || info.bmHeight == 0 {
            return Err(Error::empty());
        }
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (width * height * 4) as usize];
        let screen = GetDC(None);
        let lines =
            GetDIBits(screen, hbitmap, 0, height as u32, Some(data.as_mut_ptr() as *mut c_void), &mut header, DIB_RGB_COLORS);
        ReleaseDC(None, screen);
        if lines == 0 {
            return Err(Error::empty());
        }
        Ok((width, height, data))
    }
}

/// Reads a 32-bit shell bitmap and normalises it to premultiplied alpha. The shell doesn't
/// document whether its alpha is premultiplied, so this checks the pixels: any colour channel
/// brighter than its alpha means straight alpha.
unsafe fn hbitmap_pixels(hbitmap: HBITMAP) -> Result<Pixels> {
    unsafe {
        let (width, height, mut data) = bitmap_bits(hbitmap)?;
        let no_alpha = data.chunks_exact(4).all(|p| p[3] == 0);
        let straight = data.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
        for p in data.chunks_exact_mut(4) {
            if no_alpha {
                p[3] = 255;
            } else if straight {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
        }
        Ok(Pixels { width: width as u32, height: height as u32, data })
    }
}

// ---- The worker thread -----------------------------------------------------------------------

/// Which of an item's icons a job is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Its icon on the dock.
    Base,
    /// The bigger one for when it's pointed at.
    Zoom,
    /// A group's item, for its pop-up.
    Child(usize),
    /// One of the first four in a group, small, for the group's icon.
    Preview(usize),
}

pub struct Job {
    /// Which load this belongs to; results from an older layout are thrown away.
    pub generation: u64,
    pub item: usize,
    pub size: u32,
    pub part: Part,
    pub sources: Vec<IconSource>,
    /// Also get this size into the cache (the hover size), so a first hover needn't wait.
    pub also: Option<u32>,
}

pub struct Done {
    pub generation: u64,
    pub item: usize,
    pub part: Part,
    pub pixels: Option<Pixels>,
}

enum Task {
    Load(Job),
    /// Keep only these icons (sources, size) in the cache; delete the rest.
    Prune(Vec<(Vec<IconSource>, u32)>),
}

/// Loads icons in the background and wakes the dock window with `message` when results arrive.
pub struct IconWorker {
    tasks: Sender<Task>,
    /// Icons from network places, made on a thread of their own (started when first needed): a
    /// server that's switched off keeps the helper waiting until it's stopped (`HELPER_LIMIT`),
    /// and every other icon mustn't wait behind it.
    network: OnceLock<Sender<Task>>,
    setup: WorkerSetup,
    pub done: Arc<Mutex<Vec<Done>>>,
}

/// What a worker thread needs.
#[derive(Clone)]
struct WorkerSetup {
    done: Arc<Mutex<Vec<Done>>>,
    cache: PathBuf,
    hwnd: isize,
    message: u32,
    cache_only: bool,
}

impl WorkerSetup {
    fn spawn(&self) -> Sender<Task> {
        let (tasks, receiver) = std::sync::mpsc::channel::<Task>();
        let s = self.clone();
        std::thread::spawn(move || worker(receiver, s.done, s.cache, s.hwnd, s.message, s.cache_only));
        tasks
    }
}

impl IconWorker {
    pub fn start(home: &Home, hwnd: HWND, message: u32, cache_only: bool) -> Self {
        let done = Arc::new(Mutex::new(Vec::new()));
        let setup = WorkerSetup { done: done.clone(), cache: home.icon_cache(), hwnd: hwnd.0 as isize, message, cache_only };
        Self { tasks: setup.spawn(), network: OnceLock::new(), setup, done }
    }

    pub fn request(&self, job: Job) {
        let queue = if on_network(&job.sources) { self.network.get_or_init(|| self.setup.spawn()) } else { &self.tasks };
        let _ = queue.send(Task::Load(job));
    }

    /// Once the queued loads finish, deletes cache files not needed for `keep`.
    pub fn prune(&self, keep: Vec<(Vec<IconSource>, u32)>) {
        let _ = self.tasks.send(Task::Prune(keep));
    }
}

/// An icon straight from the cache, if it's there.
pub fn cached(cache: &Path, sources: &[IconSource], size: u32) -> Option<Pixels> {
    read_cached(&cache.join(format!("{}.bgra", cache_key(sources, size))), size)
}

/// The dock's side: icons come only from the cache. What's missing is made by a short-lived
/// helper process (`make_with_helper`), so Windows' icon machinery (~11 MB, kept for good once
/// loaded) and any shell extension's bugs stay out of the dock.
fn worker(tasks: Receiver<Task>, done: Arc<Mutex<Vec<Done>>>, cache: PathBuf, hwnd: isize, message: u32, cache_only: bool) {
    let _ = std::fs::create_dir_all(&cache);
    let deliver = |results: Vec<Done>| {
        if results.is_empty() {
            return;
        }
        if let Ok(mut list) = done.lock() {
            list.extend(results);
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), message, WPARAM(0), LPARAM(0));
        }
    };
    while let Ok(first) = tasks.recv() {
        // Take everything already waiting, so a whole relayout's icons go in one batch.
        let mut batch = vec![first];
        while let Ok(more) = tasks.try_recv() {
            batch.push(more);
        }
        let (mut ready, mut missing, mut extra, mut prunes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for task in batch {
            match task {
                Task::Load(job) => {
                    if let Some(size) = job.also.filter(|&size| !cache_only && !cached_exists(&cache, &job.sources, size)) {
                        extra.push((job.sources.clone(), size));
                    }
                    match cached(&cache, &job.sources, job.size) {
                        Some(pixels) => ready.push(Done { generation: job.generation, item: job.item, part: job.part, pixels: Some(pixels) }),
                        None if cache_only => ready.push(Done { generation: job.generation, item: job.item, part: job.part, pixels: None }),
                        None => missing.push(job),
                    }
                }
                Task::Prune(keep) => prunes.push(keep),
            }
        }
        deliver(ready);
        if !missing.is_empty() || !extra.is_empty() {
            let mut wanted: Vec<(Vec<IconSource>, u32)> = missing.iter().map(|job| (job.sources.clone(), job.size)).collect();
            wanted.extend(extra);
            // Several reloads' worth can be waiting: each icon once.
            let mut seen = HashSet::new();
            wanted.retain(|(sources, size)| seen.insert(cache_key(sources, *size)));
            make_with_helper(&wanted, &cache);
            let made = missing
                .into_iter()
                .map(|job| {
                    let pixels = cached(&cache, &job.sources, job.size);
                    if pixels.is_none() {
                        log_warn!("no icon from {:?}; showing a placeholder", job.sources);
                    }
                    Done { generation: job.generation, item: job.item, part: job.part, pixels }
                })
                .collect();
            deliver(made);
        }
        for keep in prunes {
            let keys = keep.iter().map(|(sources, size)| cache_key(sources, *size)).collect();
            prune_cache(&cache, &keys);
        }
    }
}

/// Whether making this icon means asking a network place (see `places::on_network`).
fn on_network(sources: &[IconSource]) -> bool {
    sources.iter().any(|source| match source {
        IconSource::Image(path) | IconSource::Resource(path, _) | IconSource::Shell(path) => places::on_network(path),
        IconSource::RecycleBin { .. } => false,
    })
}

fn cached_exists(cache: &Path, sources: &[IconSource], size: u32) -> bool {
    cache.join(format!("{}.bgra", cache_key(sources, size))).is_file()
}

// ---- The helper process ----------------------------------------------------------------------

/// How long the helper may go without making another icon before it's stopped (a shell
/// extension that hangs, say). Icons not made show a placeholder; the dock carries on. Counted
/// per icon, so a long list on a slow PC still gets through.
const HELPER_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Runs `desktop-dock.exe --make-icons <list> <cache>` for these icons and waits for it.
fn make_with_helper(wanted: &[(Vec<IconSource>, u32)], cache: &Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    let list = std::env::temp_dir().join(format!("desktop-dock-icons-{}-{}.txt", std::process::id(), unique()));
    let text: String = wanted.iter().map(|(sources, size)| encode_job(*size, sources) + "\n").collect();
    if std::fs::write(&list, text).is_err() {
        log_warn!("couldn't hand icons to the helper");
        return;
    }
    let (mut made, mut progress, mut counted) = (0, std::time::Instant::now(), std::time::Instant::now());
    match std::process::Command::new(exe).arg("--make-icons").arg(&list).arg(cache).spawn() {
        Ok(mut child) => loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    // Each icon it makes lands in the cache: that's its progress.
                    if counted.elapsed() >= std::time::Duration::from_secs(1) {
                        counted = std::time::Instant::now();
                        let now_made = wanted.iter().filter(|(sources, size)| cached_exists(cache, sources, *size)).count();
                        if now_made > made {
                            (made, progress) = (now_made, counted);
                        }
                    }
                    if progress.elapsed() < HELPER_LIMIT {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        continue;
                    }
                    log_warn!("the icon helper made no icon for {} s ({made} of {} made); stopped it", HELPER_LIMIT.as_secs(), wanted.len());
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(_) => {
                    let _ = child.kill();
                    break;
                }
            }
        },
        Err(e) => log_warn!("couldn't start the icon helper: {e}"),
    }
    let _ = std::fs::remove_file(&list);
}

fn unique() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The helper's whole job (`--make-icons <list> <cache>`): make each listed icon into the cache.
/// Runs in its own short-lived process, with its own COM.
pub fn make_icons(list: &Path, cache: &Path) -> usize {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    let Ok(loader) = IconLoader::new(Some(cache.to_path_buf())) else { return 0 };
    let text = std::fs::read_to_string(list).unwrap_or_default();
    text.lines().filter_map(decode_job).filter(|(size, sources)| loader.load(sources, *size).is_some()).count()
}

/// One icon to make, as a line: the size, then each source, tab-separated (Windows paths
/// can't contain tabs or line breaks).
pub fn encode_job(size: u32, sources: &[IconSource]) -> String {
    let mut line = size.to_string();
    for source in sources {
        line.push('\t');
        line.push_str(&match source {
            IconSource::Image(path) => format!("I:{path}"),
            IconSource::Resource(path, index) => format!("R:{index}:{path}"),
            IconSource::Shell(target) => format!("S:{target}"),
            IconSource::RecycleBin { full } => format!("B:{}", u8::from(*full)),
        });
    }
    line
}

pub fn decode_job(line: &str) -> Option<(u32, Vec<IconSource>)> {
    let mut fields = line.split('\t');
    let size = fields.next()?.trim().parse().ok()?;
    let sources = fields
        .map(|field| match field.split_once(':')? {
            ("I", path) => Some(IconSource::Image(path.into())),
            ("R", rest) => {
                let (index, path) = rest.split_once(':')?;
                Some(IconSource::Resource(path.into(), index.parse().ok()?))
            }
            ("S", target) => Some(IconSource::Shell(target.into())),
            ("B", full) => Some(IconSource::RecycleBin { full: full == "1" }),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (!sources.is_empty()).then_some((size, sources))
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A test icon: `n` pixels of each (colour, alpha), premultiplied.
    fn icon(parts: &[([u8; 3], u8, usize)]) -> Pixels {
        let mut data = Vec::new();
        for &([r, g, b], a, n) in parts {
            let pre = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
            for _ in 0..n {
                data.extend_from_slice(&[pre(b), pre(g), pre(r), a]);
            }
        }
        let width = data.len() as u32 / 4;
        Pixels { width, height: 1, data }
    }

    #[test]
    fn an_icons_main_colour_is_its_strongest_vivid_hue_brightened() {
        let red = main_colour(&icon(&[([220, 30, 30], 255, 100)])).unwrap();
        assert!(red[0] > 0.8 && red[1] < 0.2 && red[2] < 0.2, "{red:?}");
        // Mostly blue with a little orange and a lot of white and see-through: blue.
        let blue = main_colour(&icon(&[([20, 90, 200], 255, 60), ([250, 140, 20], 255, 15), ([255, 255, 255], 255, 80), ([0, 0, 0], 0, 200)])).unwrap();
        assert!(blue[2] > blue[0] && blue[2] > blue[1], "{blue:?}");
        // A dark navy still glows: brightened.
        let navy = main_colour(&icon(&[([10, 30, 90], 255, 50)])).unwrap();
        assert!((navy[2] - 0.85).abs() < 0.02, "{navy:?}");
        // Greys, black and white have no colour of their own; a speck of colour isn't enough.
        assert_eq!(main_colour(&icon(&[([128, 128, 128], 255, 50), ([255, 255, 255], 255, 50), ([0, 0, 0], 255, 50)])), None);
        assert_eq!(main_colour(&icon(&[([200, 200, 200], 255, 100), ([255, 0, 0], 255, 2)])), None);
        assert_eq!(main_colour(&icon(&[([255, 0, 0], 0, 100)])), None, "see-through pixels don't count");
    }

    fn item(icon: &str, target: &str) -> ItemConfig {
        let mut item = ItemConfig::default();
        item.icon = icon.into();
        item.target = target.into();
        item
    }

    #[test]
    fn icon_refs_split_file_and_number() {
        assert_eq!(parse_icon_ref(r"C:\Windows\System32\imageres.dll,-109"), (r"C:\Windows\System32\imageres.dll".into(), Some(-109)));
        assert_eq!(parse_icon_ref(r"C:\x\app.exe,3"), (r"C:\x\app.exe".into(), Some(3)));
        assert_eq!(parse_icon_ref(r"C:\Pictures\icon.png"), (r"C:\Pictures\icon.png".into(), None));
        // A comma inside a folder name isn't an icon number.
        assert_eq!(parse_icon_ref(r"C:\a,b\icon.png"), (r"C:\a,b\icon.png".into(), None));
    }

    #[test]
    fn custom_image_first_then_the_targets_own_icon() {
        let sources = sources_for(&item(r"C:\Pictures\tb.png", r"C:\tb.exe"), false);
        assert_eq!(sources, vec![IconSource::Image(r"C:\Pictures\tb.png".into()), IconSource::Shell(r"C:\tb.exe".into())]);
    }

    #[test]
    fn icons_inside_files_use_the_resource_route() {
        let sources = sources_for(&item(r"C:\Windows\System32\imageres.dll,-113", r"C:\Users\alex\Pictures"), false);
        assert_eq!(sources[0], IconSource::Resource(r"C:\Windows\System32\imageres.dll".into(), -113));
        // An exe given without a number means its first icon.
        assert_eq!(sources_for(&item(r"C:\x\app.exe", ""), false)[0], IconSource::Resource(r"C:\x\app.exe".into(), 0));
    }

    #[test]
    fn recycle_bin_follows_empty_and_full() {
        let mut bin = ItemConfig::default();
        bin.kind = Kind::RecycleBin;
        assert_eq!(sources_for(&bin, false)[0], IconSource::RecycleBin { full: false });
        assert_eq!(sources_for(&bin, true)[0], IconSource::RecycleBin { full: true });
    }

    #[test]
    fn jobs_for_the_helper_survive_the_trip() {
        let sources = vec![
            IconSource::Image(r"C:\Users\alex\Pictures\Icons\a b.png".into()),
            IconSource::Resource(r"C:\Windows\System32\imageres.dll".into(), -109),
            IconSource::Shell(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into()),
            IconSource::RecycleBin { full: true },
        ];
        let line = encode_job(108, &sources);
        assert_eq!(decode_job(&line), Some((108, sources)));
        assert_eq!(decode_job("80\tS:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), Some((80, vec![IconSource::Shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into())])));
        assert_eq!(decode_job("nonsense"), None);
        assert_eq!(decode_job("80\tX:what"), None);
    }
    #[test]
    fn cache_key_depends_on_size_and_source() {
        let a = vec![IconSource::Shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into())];
        let b = vec![IconSource::Shell("::{645FF040-5081-101B-9F08-00AA002F954E}".into())];
        assert_eq!(cache_key(&a, 80), cache_key(&a, 80), "stable");
        assert_ne!(cache_key(&a, 80), cache_key(&a, 96));
        assert_ne!(cache_key(&a, 80), cache_key(&b, 80));
    }

    #[test]
    fn cache_key_changes_when_the_source_file_changes() {
        let file = std::env::temp_dir().join(format!("dock-icon-key-{}.png", std::process::id()));
        std::fs::write(&file, b"one").unwrap();
        let sources = vec![IconSource::Image(file.to_string_lossy().into_owned())];
        let before = cache_key(&sources, 80);
        std::fs::write(&file, b"one plus more").unwrap();
        assert_ne!(cache_key(&sources, 80), before);
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn cache_files_round_trip_and_reject_wrong_sizes() {
        let file = std::env::temp_dir().join(format!("dock-icon-cache-{}.bgra", std::process::id()));
        let pixels = Pixels { width: 2, height: 2, data: (0..16).collect() };
        write_cached(&file, &pixels);
        let back = read_cached(&file, 2).unwrap();
        assert_eq!(back.data, pixels.data);
        assert!(read_cached(&file, 3).is_none(), "different size");
        std::fs::write(&file, b"DDI1junk").unwrap();
        assert!(read_cached(&file, 2).is_none(), "truncated file");
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn icons_from_network_places_are_told_apart() {
        let shell = |path: &str| vec![IconSource::Shell(path.into())];
        assert!(on_network(&shell(r"\\nas\share\tool.exe")));
        assert!(on_network(&shell("//nas/share/tool.exe")));
        assert!(on_network(&shell(r"\\?\UNC\nas\share\tool.exe")));
        assert!(on_network(&[IconSource::Resource(r"\\nas\share\icons.dll".into(), 3)]));
        assert!(!on_network(&shell(r"C:\Windows\notepad.exe")));
        assert!(!on_network(&shell(r"\\?\C:\Windows\notepad.exe")));
        assert!(!on_network(&shell(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App")));
        assert!(!on_network(&shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")));
        assert!(!on_network(&[IconSource::RecycleBin { full: false }]));
    }
}
