//! Minimal reader for the two `.npy` support files shipped with the model (1-D float32 or float64, C order).
//! ponytail: not a general npy parser; the model zip only ever contains these two shapes/dtypes.
use anyhow::{bail, Context, Result};
use std::path::Path;

pub fn load_f32_1d(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < 10 || &bytes[..6] != b"\x93NUMPY" {
        bail!("{} is not an npy file", path.display());
    }
    let (major, header_len_bytes) = (bytes[6], if bytes[6] == 1 { 2 } else { 4 });
    let hlen = if major == 1 {
        u16::from_le_bytes([bytes[8], bytes[9]]) as usize
    } else {
        u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize
    };
    let start = 8 + header_len_bytes;
    let header = std::str::from_utf8(&bytes[start..start + hlen])?;
    let data = &bytes[start + hlen..];
    if header.contains("'fortran_order': True") {
        bail!("{}: fortran order not supported", path.display());
    }
    if header.contains("'<f4'") {
        Ok(data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    } else if header.contains("'<f8'") {
        Ok(data
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) as f32)
            .collect())
    } else {
        bail!("{}: unsupported dtype in header {header}", path.display())
    }
}
