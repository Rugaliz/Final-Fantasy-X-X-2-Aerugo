//! VP8 decoding through libvpx, loaded at run time (so builds do not need it). The FFI is limited to the five
//! decoder calls and the image layout of libvpx 1.x (decoder ABI 12).

use std::ffi::c_void;
use std::os::raw::{c_char, c_int, c_long, c_uint};

use libloading::{Library, Symbol};

const DECODER_ABI_VERSION: c_int = 12;
const IMG_FMT_I420: c_int = 0x100 | 2;

#[repr(C)]
struct VpxImage {
    fmt: c_int,
    cs: c_int,
    range: c_int,
    w: c_uint,
    h: c_uint,
    bit_depth: c_uint,
    d_w: c_uint,
    d_h: c_uint,
    r_w: c_uint,
    r_h: c_uint,
    x_chroma_shift: c_uint,
    y_chroma_shift: c_uint,
    planes: [*mut u8; 4],
    stride: [c_int; 4],
    bps: c_int,
    user_priv: *mut c_void,
    img_data: *mut u8,
    img_data_owner: c_int,
    self_allocd: c_int,
    fb_priv: *mut c_void,
}

type InitFn = unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void, c_long, c_int) -> c_int;
type IfaceFn = unsafe extern "C" fn() -> *const c_void;
type DecodeFn = unsafe extern "C" fn(*mut c_void, *const u8, c_uint, *mut c_void, c_long) -> c_int;
type GetFrameFn = unsafe extern "C" fn(*mut c_void, *mut *const c_void) -> *const VpxImage;
type DestroyFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type ErrorFn = unsafe extern "C" fn(*const c_void) -> *const c_char;

#[derive(Debug, thiserror::Error)]
pub enum VpxError {
    #[error("libvpx not found ({0}); install libvpx to play movies")]
    Missing(String),
    #[error("libvpx: {0}")]
    Codec(String),
}

/// One decoded picture in I420 (planar YUV 4:2:0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Picture {
    /// Full-range RGBA8 using BT.601 limited-range coefficients (what VP8 movies use).
    pub fn to_rgba(&self, out: &mut Vec<u8>) {
        let (w, h) = (self.width as usize, self.height as usize);
        let cw = w.div_ceil(2);
        out.resize(w * h * 4, 255);
        for row in 0..h {
            for col in 0..w {
                let y = i32::from(self.y[row * w + col]) - 16;
                let ci = (row / 2) * cw + col / 2;
                let u = i32::from(self.u[ci]) - 128;
                let v = i32::from(self.v[ci]) - 128;
                let c = 298 * y;
                let r = (c + 409 * v + 128) >> 8;
                let g = (c - 100 * u - 208 * v + 128) >> 8;
                let b = (c + 516 * u + 128) >> 8;
                let o = (row * w + col) * 4;
                out[o] = r.clamp(0, 255) as u8;
                out[o + 1] = g.clamp(0, 255) as u8;
                out[o + 2] = b.clamp(0, 255) as u8;
                out[o + 3] = 255;
            }
        }
    }
}

/// A VP8 decoder instance.
pub struct Vp8Decoder {
    _lib: Library,
    ctx: Box<[u64; 64]>,
    decode: DecodeFn,
    get_frame: GetFrameFn,
    destroy: DestroyFn,
    error: Option<ErrorFn>,
}

impl std::fmt::Debug for Vp8Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp8Decoder").finish_non_exhaustive()
    }
}

const LIBRARY_NAMES: &[&str] = &[
    "libvpx.so.12",
    "libvpx.so.11",
    "libvpx.so.9",
    "libvpx.so.8",
    "libvpx.so",
    "vpx.dll",
    "libvpx-1.dll",
    "libvpx.dylib",
];

impl Vp8Decoder {
    pub fn new() -> Result<Vp8Decoder, VpxError> {
        let mut last = String::new();
        // SAFETY: loading libvpx runs its initialisers, which have no preconditions.
        let lib = LIBRARY_NAMES.iter().find_map(|name| match unsafe { Library::new(name) } {
            Ok(l) => Some(l),
            Err(e) => {
                last = e.to_string();
                None
            }
        });
        let lib = lib.ok_or(VpxError::Missing(last))?;
        // SAFETY: the symbols are libvpx's public C API with the signatures declared above (libvpx 1.x).
        unsafe {
            let sym = |name: &[u8]| -> Result<*const c_void, VpxError> {
                let s: Symbol<*const c_void> = lib.get(name).map_err(|e| VpxError::Missing(e.to_string()))?;
                Ok(*s)
            };
            let init: InitFn = std::mem::transmute(sym(b"vpx_codec_dec_init_ver\0")?);
            let iface: IfaceFn = std::mem::transmute(sym(b"vpx_codec_vp8_dx\0")?);
            let decode: DecodeFn = std::mem::transmute(sym(b"vpx_codec_decode\0")?);
            let get_frame: GetFrameFn = std::mem::transmute(sym(b"vpx_codec_get_frame\0")?);
            let destroy: DestroyFn = std::mem::transmute(sym(b"vpx_codec_destroy\0")?);
            let error: Option<ErrorFn> = sym(b"vpx_codec_error\0").ok().map(|p| std::mem::transmute(p));
            // vpx_codec_ctx_t is 56 bytes on 64-bit targets; a zeroed, larger buffer is safe.
            let mut ctx = Box::new([0u64; 64]);
            let rc = init(ctx.as_mut_ptr().cast(), iface(), std::ptr::null(), 0, DECODER_ABI_VERSION);
            if rc != 0 {
                return Err(VpxError::Codec(format!("init failed ({rc})")));
            }
            Ok(Vp8Decoder { _lib: lib, ctx, decode, get_frame, destroy, error })
        }
    }

    fn last_error(&self) -> String {
        match self.error {
            // SAFETY: the context is initialised; libvpx returns a static string.
            Some(f) => unsafe {
                let p = f(self.ctx.as_ptr().cast());
                if p.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            },
            None => String::new(),
        }
    }

    /// Decode one compressed frame; returns the picture if one is ready.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, VpxError> {
        // SAFETY: the context is initialised; `data` outlives the call; libvpx copies what it keeps.
        let rc = unsafe {
            (self.decode)(
                self.ctx.as_mut_ptr().cast(),
                data.as_ptr(),
                data.len() as c_uint,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(VpxError::Codec(self.last_error()));
        }
        let mut iter: *const c_void = std::ptr::null();
        // SAFETY: as above; the returned image stays valid until the next decode call, and we copy it now.
        let img = unsafe { (self.get_frame)(self.ctx.as_mut_ptr().cast(), &mut iter) };
        if img.is_null() {
            return Ok(None);
        }
        // SAFETY: non-null image from libvpx with the layout declared above.
        let img = unsafe { &*img };
        if img.fmt != IMG_FMT_I420 {
            return Err(VpxError::Codec(format!("unexpected image format {:#x}", img.fmt)));
        }
        let (w, h) = (img.d_w as usize, img.d_h as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let copy = |plane: usize, pw: usize, ph: usize| -> Vec<u8> {
            let stride = img.stride[plane] as usize;
            let mut out = Vec::with_capacity(pw * ph);
            for row in 0..ph {
                // SAFETY: libvpx guarantees `ph` rows of at least `pw` bytes at `stride` spacing.
                let src = unsafe { std::slice::from_raw_parts(img.planes[plane].add(row * stride), pw) };
                out.extend_from_slice(src);
            }
            out
        };
        Ok(Some(Picture {
            width: w as u32,
            height: h as u32,
            y: copy(0, w, h),
            u: copy(1, cw, ch),
            v: copy(2, cw, ch),
        }))
    }
}

impl Drop for Vp8Decoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `new` and is destroyed once.
        unsafe { (self.destroy)(self.ctx.as_mut_ptr().cast()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_converts_to_grey() {
        let p = Picture { width: 2, height: 2, y: vec![126; 4], u: vec![128], v: vec![128] };
        let mut out = Vec::new();
        p.to_rgba(&mut out);
        assert_eq!(&out[..4], &[128, 128, 128, 255]);
    }

    #[test]
    fn decoder_loads_or_reports_missing() {
        match Vp8Decoder::new() {
            Ok(mut d) => {
                // Garbage input must fail cleanly, not crash.
                let _ = d.decode(&[0, 0, 0]);
            }
            Err(VpxError::Missing(m)) => eprintln!("libvpx not installed: {m}"),
            Err(e) => panic!("{e}"),
        }
    }
}
