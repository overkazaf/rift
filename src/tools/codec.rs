const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(B64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let input: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if input.len() % 4 != 0 {
        return Err("invalid base64: length not multiple of 4".into());
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    for chunk in input.chunks(4) {
        let mut vals = [0u32; 4];
        let mut pad = 0;
        for (i, &b) in chunk.iter().enumerate() {
            vals[i] = match b {
                b'A'..=b'Z' => (b - b'A') as u32,
                b'a'..=b'z' => (b - b'a' + 26) as u32,
                b'0'..=b'9' => (b - b'0' + 52) as u32,
                b'+' => 62,
                b'/' => 63,
                b'=' => { pad += 1; 0 }
                _ => return Err(format!("invalid base64 character: {}", b as char)),
            };
        }
        let triple = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
        out.push(((triple >> 16) & 0xFF) as u8);
        if pad < 2 {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if pad < 1 {
            out.push((triple & 0xFF) as u8);
        }
    }
    Ok(out)
}

pub fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX_UPPER[(b >> 4) as usize] as char);
                out.push(HEX_UPPER[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

pub fn url_decode(input: &str) -> Result<String, String> {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("incomplete percent encoding".into());
            }
            let hi = hex_val(bytes[i + 1]).ok_or("invalid hex digit")?;
            let lo = hex_val(bytes[i + 2]).ok_or("invalid hex digit")?;
            out.push((hi << 4) | lo);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| format!("invalid utf-8: {e}"))
}

pub fn hex_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for &b in input {
        out.push(HEX_LOWER[(b >> 4) as usize] as char);
        out.push(HEX_LOWER[(b & 0xf) as usize] as char);
    }
    out
}

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

pub fn hex_decode(input: &str) -> Result<Vec<u8>, String> {
    let input: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if input.len() % 2 != 0 {
        return Err("hex string must have even length".into());
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    for pair in input.chunks(2) {
        let hi = hex_val(pair[0]).ok_or_else(|| format!("invalid hex digit: {}", pair[0] as char))?;
        let lo = hex_val(pair[1]).ok_or_else(|| format!("invalid hex digit: {}", pair[1] as char))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
