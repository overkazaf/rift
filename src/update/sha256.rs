//! Small pure-Rust SHA-256 (FIPS 180-4) for verifying release downloads, plus
//! a parser for `.sha256` checksum files (`shasum -a 256` / `sha256sum` / BSD).

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/// Streaming SHA-256 hasher.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self { state: H0, buf: [0; 64], buf_len: 0, total: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                compress(&mut self.state, &block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            compress(&mut self.state, block.try_into().unwrap());
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let rem = (self.total as usize + 1) % 64;
        let zeros = if rem <= 56 { 56 - rem } else { 120 - rem };
        pad.extend(std::iter::repeat(0).take(zeros));
        pad.extend_from_slice(&bits.to_be_bytes());
        let total = self.total;
        self.update(&pad);
        self.total = total;
        debug_assert_eq!(self.buf_len, 0);
        let mut out = [0u8; 32];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    pub fn finish_hex(self) -> String {
        hex(&self.finish())
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One-shot digest as lowercase hex.
#[cfg(test)]
pub fn digest_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finish_hex()
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extract the expected digest for `file_name` from a checksum file.
///
/// Accepts `<hex>  <name>` / `<hex> *<name>` (GNU / shasum), `SHA256 (<name>) = <hex>`
/// (BSD) and a bare `<hex>`. Lines naming a different file are ignored; a
/// line without a name matches anything. Returns lowercase hex.
pub fn parse_checksum_file(text: &str, file_name: &str) -> Option<String> {
    let base = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // BSD style: SHA256 (file) = hex
        if let Some(rest) = line.strip_prefix("SHA256 (").or_else(|| line.strip_prefix("SHA2-256 (")) {
            if let Some((name, hexpart)) = rest.rsplit_once(") = ") {
                let hexpart = hexpart.trim();
                if is_digest(hexpart) && base(name) == file_name {
                    return Some(hexpart.to_ascii_lowercase());
                }
            }
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let digest = parts.next().unwrap_or("");
        if !is_digest(digest) {
            continue;
        }
        let name = parts.next().map(|n| n.trim().trim_start_matches('*')).unwrap_or("");
        if name.is_empty() || base(name) == file_name {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(digest_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(digest_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            digest_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            digest_hex(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
        // One million 'a's (multi-block, exercises length encoding).
        assert_eq!(
            digest_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streaming_matches_one_shot_at_every_split() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 + 3) as u8).collect();
        let want = digest_hex(&data);
        for split in [0, 1, 55, 56, 63, 64, 65, 127, 128, 200, 300] {
            let mut h = Sha256::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish_hex(), want, "split at {split}");
        }
        // Byte-by-byte.
        let mut h = Sha256::new();
        for b in &data {
            h.update(std::slice::from_ref(b));
        }
        assert_eq!(h.finish_hex(), want);
    }

    #[test]
    fn checksum_file_formats() {
        let d = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let up = d.to_ascii_uppercase();
        let f = "Rift-0.4.1-macos.dmg";
        assert_eq!(parse_checksum_file(&format!("{d}  {f}\n"), f).as_deref(), Some(d));
        assert_eq!(parse_checksum_file(&format!("{up} *{f}"), f).as_deref(), Some(d), "binary marker + uppercase");
        assert_eq!(parse_checksum_file(&format!("{d}  dist/{f}"), f).as_deref(), Some(d), "path prefix");
        assert_eq!(parse_checksum_file(&format!("{d}\n"), f).as_deref(), Some(d), "bare digest");
        assert_eq!(parse_checksum_file(&format!("SHA256 ({f}) = {d}"), f).as_deref(), Some(d), "BSD style");
        let other = "0000000000000000000000000000000000000000000000000000000000000000";
        let multi = format!("{other}  rift-0.4.1-linux-x86_64.tar.gz\n{d}  {f}\n");
        assert_eq!(parse_checksum_file(&multi, f).as_deref(), Some(d), "picks the matching line");
        assert_eq!(parse_checksum_file(&format!("{d}  other.dmg"), f), None, "wrong file");
        assert_eq!(parse_checksum_file("not a checksum", f), None);
        assert_eq!(parse_checksum_file(&format!("{}  {f}", &d[..63]), f), None, "short digest");
        assert_eq!(parse_checksum_file("", f), None);
    }
}
