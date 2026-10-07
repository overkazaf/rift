pub fn hex_dump(data: &[u8], _cols: usize) -> String {
    let addr_color = "\x1b[90m";
    let printable_color = "\x1b[32m";
    let non_printable_color = "\x1b[31m";
    let ascii_color = "\x1b[36m";
    let dim = "\x1b[90m";
    let reset = "\x1b[0m";

    let mut out = String::with_capacity(data.len() * 5);

    for (chunk_idx, chunk) in data.chunks(16).enumerate() {
        let offset = chunk_idx * 16;

        out.push_str(&format!("{addr_color}{offset:08x}{reset}  "));

        for (i, &byte) in chunk.iter().enumerate() {
            let color = if byte.is_ascii_graphic() || byte == b' ' {
                printable_color
            } else {
                non_printable_color
            };
            out.push_str(&format!("{color}{byte:02x}{reset} "));
            if i == 7 {
                out.push(' ');
            }
        }

        if chunk.len() < 16 {
            for i in chunk.len()..16 {
                out.push_str("   ");
                if i == 7 {
                    out.push(' ');
                }
            }
        }

        out.push_str(&format!(" {dim}|{reset}{ascii_color}"));
        for &byte in chunk {
            if byte.is_ascii_graphic() || byte == b' ' {
                out.push(byte as char);
            } else {
                out.push('.');
            }
        }
        for _ in chunk.len()..16 {
            out.push(' ');
        }
        out.push_str(&format!("{reset}{dim}|{reset}\r\n"));
    }

    if !data.is_empty() {
        out.push_str(&format!(
            "\r\n{dim}{} bytes ({:#x}){reset}\r\n",
            data.len(),
            data.len()
        ));
    }

    out
}
