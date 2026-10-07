use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub fn encode_key(event: &KeyEvent, modifiers: ModifiersState, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = modifiers.control_key();
    let alt = modifiers.alt_key();

    if let Key::Named(named) = &event.logical_key {
        let seq = match named {
            NamedKey::Enter => Some(b"\r".to_vec()),
            NamedKey::Backspace => Some(vec![0x7f]),
            NamedKey::Tab => Some(b"\t".to_vec()),
            NamedKey::Escape => Some(vec![0x1b]),
            NamedKey::ArrowUp => Some(arrow_key(b'A', app_cursor)),
            NamedKey::ArrowDown => Some(arrow_key(b'B', app_cursor)),
            NamedKey::ArrowRight => Some(arrow_key(b'C', app_cursor)),
            NamedKey::ArrowLeft => Some(arrow_key(b'D', app_cursor)),
            NamedKey::Home => Some(b"\x1b[H".to_vec()),
            NamedKey::End => Some(b"\x1b[F".to_vec()),
            NamedKey::PageUp => Some(b"\x1b[5~".to_vec()),
            NamedKey::PageDown => Some(b"\x1b[6~".to_vec()),
            NamedKey::Insert => Some(b"\x1b[2~".to_vec()),
            NamedKey::Delete => Some(b"\x1b[3~".to_vec()),
            NamedKey::F1 => Some(b"\x1bOP".to_vec()),
            NamedKey::F2 => Some(b"\x1bOQ".to_vec()),
            NamedKey::F3 => Some(b"\x1bOR".to_vec()),
            NamedKey::F4 => Some(b"\x1bOS".to_vec()),
            NamedKey::F5 => Some(b"\x1b[15~".to_vec()),
            NamedKey::F6 => Some(b"\x1b[17~".to_vec()),
            NamedKey::F7 => Some(b"\x1b[18~".to_vec()),
            NamedKey::F8 => Some(b"\x1b[19~".to_vec()),
            NamedKey::F9 => Some(b"\x1b[20~".to_vec()),
            NamedKey::F10 => Some(b"\x1b[21~".to_vec()),
            NamedKey::F11 => Some(b"\x1b[23~".to_vec()),
            NamedKey::F12 => Some(b"\x1b[24~".to_vec()),
            _ => None,
        };
        if seq.is_some() {
            return seq;
        }
    }

    if let Some(text) = &event.text {
        let s = text.as_str();
        if !s.is_empty() {
            if ctrl {
                if let Some(c) = s.chars().next() {
                    let ctrl_byte = match c {
                        'a'..='z' => Some(c as u8 - b'a' + 1),
                        '@' => Some(0),
                        '[' => Some(0x1b),
                        '\\' => Some(0x1c),
                        ']' => Some(0x1d),
                        '^' => Some(0x1e),
                        '_' => Some(0x1f),
                        _ => None,
                    };
                    if let Some(b) = ctrl_byte {
                        return Some(vec![b]);
                    }
                }
            }
            if alt {
                let mut bytes = vec![0x1b];
                bytes.extend_from_slice(s.as_bytes());
                return Some(bytes);
            }
            return Some(s.as_bytes().to_vec());
        }
    }

    None
}

fn arrow_key(code: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', code]
    } else {
        vec![0x1b, b'[', code]
    }
}
