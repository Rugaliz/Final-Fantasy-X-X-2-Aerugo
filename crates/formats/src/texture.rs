//! Textures stored in Phyre clusters (`*.dds.phyre`): format, size and mip levels. Pixel data follows the
//! cluster's fixups, level after level, largest first.

use std::ops::Range;

use crate::phyre::{Cluster, ObjectId, Pointer};

/// Pixel formats named by the clusters' `m_format` strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// BC1 / DXT1, 8 bytes per 4×4 block.
    Bc1,
    /// BC2 / DXT3.
    Bc2,
    /// BC3 / DXT5.
    Bc3,
    /// 8-bit RGBA, bytes in R, G, B, A order.
    Rgba8,
    /// 8-bit BGRA, bytes in B, G, R, A order.
    Bgra8,
    /// One 8-bit channel (luminance or alpha).
    R8,
    /// Two 8-bit channels.
    Rg8,
    Rgba16F,
    Rgba32F,
    R32F,
}

impl PixelFormat {
    pub fn from_name(name: &str) -> Option<PixelFormat> {
        Some(match name {
            "DXT1" => PixelFormat::Bc1,
            "DXT3" => PixelFormat::Bc2,
            "DXT5" => PixelFormat::Bc3,
            "RGBA8" => PixelFormat::Rgba8,
            "ARGB8" => PixelFormat::Bgra8,
            "L8" | "A8" => PixelFormat::R8,
            "LA8" => PixelFormat::Rg8,
            "RGBA16F" => PixelFormat::Rgba16F,
            "RGBA32F" => PixelFormat::Rgba32F,
            "R32F" => PixelFormat::R32F,
            _ => return None,
        })
    }

    pub fn is_block_compressed(self) -> bool {
        matches!(self, PixelFormat::Bc1 | PixelFormat::Bc2 | PixelFormat::Bc3)
    }

    /// Bytes per 4×4 block (compressed) or per pixel.
    pub fn unit_bytes(self) -> usize {
        match self {
            PixelFormat::Bc1 => 8,
            PixelFormat::Bc2 | PixelFormat::Bc3 => 16,
            PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::R32F => 4,
            PixelFormat::R8 => 1,
            PixelFormat::Rg8 => 2,
            PixelFormat::Rgba16F => 8,
            PixelFormat::Rgba32F => 16,
        }
    }

    /// Bytes of one level.
    pub fn level_size(self, width: u32, height: u32) -> usize {
        let (w, h) = (width.max(1) as usize, height.max(1) as usize);
        if self.is_block_compressed() {
            w.div_ceil(4) * h.div_ceil(4) * self.unit_bytes()
        } else {
            w * h * self.unit_bytes()
        }
    }

    /// Bytes per row of 4×4 blocks or pixels, as an upload needs it.
    pub fn row_pitch(self, width: u32) -> usize {
        let w = width.max(1) as usize;
        if self.is_block_compressed() { w.div_ceil(4) * self.unit_bytes() } else { w * self.unit_bytes() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    /// Asset name from the cluster's asset reference (source path of the texture).
    pub name: String,
    /// The format string as stored.
    pub format_name: String,
    pub format: Option<PixelFormat>,
    pub width: u32,
    pub height: u32,
    /// Faces: 1, or 6 for cube maps.
    pub faces: u32,
    /// Byte ranges of each level in [`Cluster::texture_data`], face by face for cube maps.
    pub levels: Vec<Range<usize>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TextureError {
    #[error("unknown pixel format {0:?}")]
    UnknownFormat(String),
    #[error(
        "{name}: {available} bytes of pixel data, {width}×{height} {format} needs {needed} for one level"
    )]
    TooSmall { name: String, format: String, width: u32, height: u32, needed: usize, available: usize },
}

/// Name of the asset reference that points at `obj`, if any.
pub fn asset_name(cluster: &Cluster, obj: ObjectId) -> Option<String> {
    cluster.objects("PAssetReference").into_iter().find_map(|r| match cluster.pointer(r, "m_asset") {
        Some(Pointer::Object { target, .. }) if target == obj => cluster.string(r, "m_id"),
        _ => None,
    })
}

/// Every texture of a cluster, with level ranges laid out one after another in the texture data.
pub fn read_textures(cluster: &Cluster) -> Vec<Result<Texture, TextureError>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let total = cluster.texture_data.len();
    for obj in cluster.objects("PTexture2D").into_iter().chain(cluster.objects("PTextureCubeMap")) {
        let cube = cluster.namespace.is_a(cluster.list(obj).class, "PTextureCubeMap");
        let name = asset_name(cluster, obj).unwrap_or_else(|| format!("texture {}", obj.index));
        let format_name = match cluster.pointer(obj, "m_format") {
            Some(Pointer::User(u)) => cluster.user_string(u),
            _ => String::new(),
        };
        let format = PixelFormat::from_name(&format_name);
        // Cube maps store one edge length.
        let edge = cluster.u32(obj, "m_size");
        let width = cluster.u32(obj, "m_width").or(edge).unwrap_or(0);
        let height = cluster.u32(obj, "m_height").or(edge).unwrap_or(0);
        let Some(pf) = format else {
            out.push(Err(TextureError::UnknownFormat(format_name)));
            continue;
        };
        let faces = if cube { 6 } else { 1 };
        // Prefer the level count that fills the data exactly; the two counters differ by one between files.
        let remaining = total - cursor.min(total);
        let chain = |levels: u32| -> usize {
            (0..levels).map(|l| pf.level_size(width >> l, height >> l)).sum::<usize>() * faces as usize
        };
        let max_levels = 32 - width.max(height).max(1).leading_zeros();
        let candidates = [
            cluster.u32(obj, "m_maxMipLevel").map(|m| m + 1),
            cluster.u32(obj, "m_mipmapCount"),
            cluster.u32(obj, "m_mipmapCount").map(|m| m + 1),
            Some(1),
        ];
        let levels = candidates
            .into_iter()
            .flatten()
            .map(|l| l.clamp(1, max_levels.max(1)))
            .find(|&l| chain(l) == remaining)
            .or_else(|| {
                candidates
                    .into_iter()
                    .flatten()
                    .map(|l| l.clamp(1, max_levels.max(1)))
                    .find(|&l| chain(l) <= remaining)
            })
            .unwrap_or(1);
        if chain(1) > remaining {
            out.push(Err(TextureError::TooSmall {
                name,
                format: format_name,
                width,
                height,
                needed: pf.level_size(width, height),
                available: remaining,
            }));
            continue;
        }
        let mut ranges = Vec::with_capacity((levels * faces) as usize);
        for _face in 0..faces {
            for l in 0..levels {
                let size = pf.level_size(width >> l, height >> l);
                ranges.push(cursor..cursor + size);
                cursor += size;
            }
        }
        out.push(Ok(Texture { name, format_name, format, width, height, faces, levels: ranges }));
    }
    out
}

/// Decode one level to RGBA8 (for thumbnails, tests and software paths). Block formats are expanded.
pub fn decode_rgba8(format: PixelFormat, width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    if data.len() < format.level_size(width, height) {
        return None;
    }
    let mut out = vec![0u8; w * h * 4];
    match format {
        PixelFormat::Rgba8 => out.copy_from_slice(&data[..w * h * 4]),
        PixelFormat::Bgra8 => {
            for (o, i) in out.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                o.copy_from_slice(&[i[2], i[1], i[0], i[3]]);
            }
        }
        PixelFormat::R8 => {
            for (o, &v) in out.chunks_exact_mut(4).zip(data) {
                o.copy_from_slice(&[v, v, v, 255]);
            }
        }
        PixelFormat::Rg8 => {
            for (o, i) in out.chunks_exact_mut(4).zip(data.chunks_exact(2)) {
                o.copy_from_slice(&[i[0], i[0], i[0], i[1]]);
            }
        }
        PixelFormat::Bc1 | PixelFormat::Bc2 | PixelFormat::Bc3 => {
            let bw = w.div_ceil(4);
            for (bi, block) in data.chunks_exact(format.unit_bytes()).take(bw * h.div_ceil(4)).enumerate() {
                let (bx, by) = (bi % bw * 4, bi / bw * 4);
                let px = decode_block(format, block);
                for y in 0..4 {
                    for x in 0..4 {
                        let (px_x, px_y) = (bx + x, by + y);
                        if px_x < w && px_y < h {
                            let o = (px_y * w + px_x) * 4;
                            out[o..o + 4].copy_from_slice(&px[y * 4 + x]);
                        }
                    }
                }
            }
        }
        PixelFormat::Rgba16F | PixelFormat::Rgba32F | PixelFormat::R32F => return None,
    }
    Some(out)
}

fn rgb565(c: u16) -> [u8; 3] {
    let r = (c >> 11 & 31) as u32;
    let g = (c >> 5 & 63) as u32;
    let b = (c & 31) as u32;
    [(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]
}

fn decode_block(format: PixelFormat, b: &[u8]) -> [[u8; 4]; 16] {
    let color = if format == PixelFormat::Bc1 { b } else { &b[8..16] };
    let c0 = u16::from_le_bytes([color[0], color[1]]);
    let c1 = u16::from_le_bytes([color[2], color[3]]);
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mix = |a: u8, b: u8, wa: u32, wb: u32| ((u32::from(a) * wa + u32::from(b) * wb) / (wa + wb)) as u8;
    let lerp = |wa, wb| [0, 1, 2].map(|i| mix(p0[i], p1[i], wa, wb));
    let four = c0 > c1 || format != PixelFormat::Bc1;
    let palette: [[u8; 4]; 4] = if four {
        let (a, b) = (lerp(2, 1), lerp(1, 2));
        [
            [p0[0], p0[1], p0[2], 255],
            [p1[0], p1[1], p1[2], 255],
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
        ]
    } else {
        let m = lerp(1, 1);
        [[p0[0], p0[1], p0[2], 255], [p1[0], p1[1], p1[2], 255], [m[0], m[1], m[2], 255], [0, 0, 0, 0]]
    };
    let indices = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
    let mut px = [[0u8; 4]; 16];
    for (i, p) in px.iter_mut().enumerate() {
        *p = palette[(indices >> (2 * i) & 3) as usize];
    }
    match format {
        PixelFormat::Bc2 => {
            for (i, p) in px.iter_mut().enumerate() {
                let nib = b[i / 2] >> (4 * (i % 2)) & 15;
                p[3] = nib * 17;
            }
        }
        PixelFormat::Bc3 => {
            let (a0, a1) = (b[0], b[1]);
            let mut alpha = [a0, a1, 0, 0, 0, 0, 0, 0];
            let (a0w, a1w) = (u32::from(a0), u32::from(a1));
            if a0 > a1 {
                for k in 1..7u32 {
                    alpha[k as usize + 1] = (((7 - k) * a0w + k * a1w) / 7) as u8;
                }
            } else {
                for k in 1..5u32 {
                    alpha[k as usize + 1] = (((5 - k) * a0w + k * a1w) / 5) as u8;
                }
                alpha[6] = 0;
                alpha[7] = 255;
            }
            let bits = u64::from_le_bytes([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0]);
            for (i, p) in px.iter_mut().enumerate() {
                p[3] = alpha[(bits >> (3 * i) & 7) as usize];
            }
        }
        _ => {}
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_sizes() {
        assert_eq!(PixelFormat::Bc1.level_size(512, 512), 131_072);
        assert_eq!(PixelFormat::Bc1.level_size(1, 1), 8);
        assert_eq!(PixelFormat::Bc3.level_size(6, 2), 32);
        assert_eq!(PixelFormat::Bgra8.level_size(3, 3), 36);
        let chain: usize = (0..10).map(|l| PixelFormat::Bc1.level_size(512 >> l, 512 >> l)).sum();
        assert_eq!(chain, 174_776);
    }

    #[test]
    fn bc1_block_decodes_endpoints() {
        // c0 = pure red, c1 = pure blue, all indices 0 except pixel 1 -> 1.
        let block = [0x00, 0xF8, 0x1F, 0x00, 0b0000_0100, 0, 0, 0];
        let px = decode_rgba8(PixelFormat::Bc1, 4, 4, &block).unwrap();
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        assert_eq!(&px[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn bc3_alpha_endpoints() {
        let mut block = [0u8; 16];
        block[0] = 255; // a0
        block[1] = 0; // a1
        block[2] = 0b0000_1000; // pixel 0 -> index 0 (255), pixel 1 -> index 1 (0)
        let px = decode_rgba8(PixelFormat::Bc3, 4, 4, &block).unwrap();
        assert_eq!(px[3], 255);
        assert_eq!(px[7], 0);
    }

    #[test]
    fn bgra_swizzles() {
        let px = decode_rgba8(PixelFormat::Bgra8, 1, 1, &[1, 2, 3, 4]).unwrap();
        assert_eq!(px, vec![3, 2, 1, 4]);
    }
}
