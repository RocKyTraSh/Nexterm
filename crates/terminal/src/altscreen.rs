//! Detects whether the terminal is showing the alternate screen buffer.
//!
//! Programs like `vim` / `htop` switch to the alt screen with
//! `ESC [ ? 1049 h` (or `1047` / `47`) and back with the `l` variant. While the
//! alt screen is active we must not inject highlighting or rewrite output —
//! this is the safety gate that keeps ncurses TUIs intact.

/// Tracks alt-screen state across a byte stream.
#[derive(Debug, Default)]
pub struct AltScreenTracker {
    in_alt: bool,
}

impl AltScreenTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn in_alt_screen(&self) -> bool {
        self.in_alt
    }

    /// Feed a chunk of raw terminal output; updates internal state.
    ///
    /// Recognizes the private-mode sequences `ESC [ ? <n> h|l` for
    /// `n in {47, 1047, 1049}`. Intentionally minimal but covers what common
    /// full-screen programs emit. (A full SGR/VT parser arrives with the GUI's
    /// terminal widget.)
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            let is_csi_private = bytes[i] == 0x1b
                && i + 2 < bytes.len()
                && bytes[i + 1] == b'['
                && bytes[i + 2] == b'?';
            if is_csi_private {
                let mut j = i + 3;
                let mut num = 0u32;
                let mut have_digit = false;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    num = num
                        .saturating_mul(10)
                        .saturating_add((bytes[j] - b'0') as u32);
                    have_digit = true;
                    j += 1;
                }
                if have_digit && j < bytes.len() && (bytes[j] == b'h' || bytes[j] == b'l') {
                    if matches!(num, 47 | 1047 | 1049) {
                        self.in_alt = bytes[j] == b'h';
                    }
                    i = j + 1;
                    continue;
                }
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enters_and_leaves_alt_screen() {
        let mut t = AltScreenTracker::new();
        assert!(!t.in_alt_screen());
        t.feed(b"\x1b[?1049h");
        assert!(t.in_alt_screen());
        t.feed(b"some full-screen drawing");
        assert!(t.in_alt_screen());
        t.feed(b"\x1b[?1049l");
        assert!(!t.in_alt_screen());
    }
}
