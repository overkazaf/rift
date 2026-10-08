//! Minimal PNG encoder: 8-bit RGBA, per-row Sub filter, zlib via `flate2`.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    table
}

fn crc32(table: &[u32; 256], kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in kind.iter().chain(data.iter()) {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn chunk(out: &mut Vec<u8>, table: &[u32; 256], kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(table, kind, data).to_be_bytes());
}

/// Encode tightly packed RGBA8 pixels (`w * h * 4` bytes) as a PNG file.
pub fn encode_rgba(w: u32, h: u32, rgba: &[u8]) -> std::io::Result<Vec<u8>> {
    assert_eq!(rgba.len(), w as usize * h as usize * 4, "pixel buffer size mismatch");
    let table = crc_table();
    let stride = w as usize * 4;

    // Sub filter: each byte minus the byte one pixel (4 bytes) to its left.
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for row in rgba.chunks_exact(stride) {
        raw.push(1);
        for (i, &b) in row.iter().enumerate() {
            let left = if i >= 4 { row[i - 4] } else { 0 };
            raw.push(b.wrapping_sub(left));
        }
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&raw)?;
    let idat = z.finish()?;

    let mut out = Vec::with_capacity(idat.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, deflate, filter 0, no interlace
    chunk(&mut out, &table, b"IHDR", &ihdr);
    chunk(&mut out, &table, b"IDAT", &idat);
    chunk(&mut out, &table, b"IEND", &[]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_known_vector() {
        // CRC-32 of "IEND" (the empty IEND chunk) is a well-known constant.
        assert_eq!(crc32(&crc_table(), b"IEND", &[]), 0xAE42_6082);
    }

    /// Decode our own output: inflate IDAT and undo the Sub filter.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        use std::io::Read;
        let (mut pos, mut idat) = (8, Vec::new());
        let (mut w, mut h) = (0, 0);
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = &png[pos + 4..pos + 8];
            let data = &png[pos + 8..pos + 8 + len];
            let crc = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&crc_table(), kind.try_into().unwrap(), data), "bad CRC");
            match kind {
                b"IHDR" => {
                    w = u32::from_be_bytes(data[0..4].try_into().unwrap());
                    h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                }
                b"IDAT" => idat.extend_from_slice(data),
                _ => {}
            }
            pos += 12 + len;
        }
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).unwrap();
        let stride = w as usize * 4;
        let mut out = Vec::new();
        for row in raw.chunks_exact(stride + 1) {
            assert_eq!(row[0], 1, "Sub filter expected");
            let start = out.len();
            for (i, &b) in row[1..].iter().enumerate() {
                let left = if i >= 4 { out[start + i - 4] } else { 0 };
                out.push(b.wrapping_add(left));
            }
        }
        (w, h, out)
    }

    #[test]
    fn roundtrips_pixels() {
        let px: Vec<u8> = (0..7 * 5 * 4).map(|i| (i * 37 + i / 5) as u8).collect();
        let png = encode_rgba(7, 5, &px).unwrap();
        let (w, h, back) = decode(&png);
        assert_eq!((w, h), (7, 5));
        assert_eq!(back, px);
    }

    #[test]
    fn encodes_valid_header_and_roundtrips_size() {
        let px: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 7) as u8).collect();
        let png = encode_rgba(4, 3, &px).unwrap();
        assert_eq!(&png[..8], &SIGNATURE);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 4);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 3);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
