//! A deliberately small terminal model: a fixed grid, a cursor, SGR attributes,
//! and scrolling. Enough to replay a cast that only ever writes lines.
//!
//! Not a terminal emulator. Cursor addressing, alternate screens, insert/delete,
//! scroll regions, tabs and OSC sequences are parsed only far enough to be
//! *skipped* cleanly, so an unexpected sequence cannot leave stray bytes on
//! screen. If a future cast needs any of them, this is the file that grows.
//!
//! Every character is treated as one cell wide, which holds for the capture
//! (ASCII, box drawing, and a few dingbats). East-Asian wide characters would
//! need a width table.

use crate::theme::{Ink, ink_for_sgr};

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub ink: Ink,
    pub bold: bool,
    pub dim: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            ink: Ink::Body,
            bold: false,
            dim: false,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Attrs {
    ink: Ink,
    bold: bool,
    dim: bool,
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    pub grid: Vec<Cell>,
    /// Bumped whenever the visible grid changes, so the renderer can reuse the
    /// previous frame instead of repainting an identical one.
    pub revision: u64,
    row: usize,
    col: usize,
    at: Attrs,
    pending: String,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        Term {
            cols,
            rows,
            grid: vec![Cell::default(); cols * rows],
            revision: 0,
            row: 0,
            col: 0,
            at: Attrs::default(),
            pending: String::new(),
        }
    }

    pub fn cell(&self, r: usize, c: usize) -> Cell {
        self.grid[r * self.cols + c]
    }

    fn scroll_up(&mut self) {
        self.grid.copy_within(self.cols.., 0);
        let last = (self.rows - 1) * self.cols;
        for c in &mut self.grid[last..] {
            *c = Cell::default();
        }
    }

    fn newline(&mut self) {
        self.row += 1;
        if self.row >= self.rows {
            self.row = self.rows - 1;
            self.scroll_up();
        }
    }

    fn put(&mut self, ch: char) {
        if self.col >= self.cols {
            self.col = 0;
            self.newline();
        }
        let i = self.row * self.cols + self.col;
        self.grid[i] = Cell {
            ch,
            ink: self.at.ink,
            bold: self.at.bold,
            dim: self.at.dim,
        };
        self.col += 1;
    }

    fn sgr(&mut self, params: &str) {
        // An empty parameter list means reset, as does an explicit 0.
        if params.is_empty() {
            self.at = Attrs::default();
            return;
        }
        for p in params.split(';') {
            let code: u16 = p.parse().unwrap_or(0);
            match code {
                0 => self.at = Attrs::default(),
                1 => self.at.bold = true,
                2 => self.at.dim = true,
                22 => {
                    self.at.bold = false;
                    self.at.dim = false;
                }
                39 => self.at.ink = Ink::Body,
                _ => {
                    if let Some(ink) = ink_for_sgr(code) {
                        self.at.ink = ink;
                    }
                }
            }
        }
    }

    /// Feed output. Chunks may split a sequence, so anything incomplete is held
    /// over until the next call.
    pub fn feed(&mut self, chunk: &str) {
        let before = self.grid_hash();
        let mut s = std::mem::take(&mut self.pending);
        s.push_str(chunk);
        let b: Vec<char> = s.chars().collect();
        let mut i = 0usize;

        while i < b.len() {
            let c = b[i];
            match c {
                '\x1b' => {
                    // Need at least the introducer to know what this is.
                    if i + 1 >= b.len() {
                        self.pending = b[i..].iter().collect();
                        break;
                    }
                    match b[i + 1] {
                        '[' => {
                            // CSI: params, then a final byte in @..~
                            let mut j = i + 2;
                            while j < b.len() && !matches!(b[j], '@'..='~') {
                                j += 1;
                            }
                            if j >= b.len() {
                                self.pending = b[i..].iter().collect();
                                break;
                            }
                            let params: String = b[i + 2..j].iter().collect();
                            if b[j] == 'm' {
                                self.sgr(&params);
                            }
                            // Every other CSI is skipped, not rendered.
                            i = j + 1;
                            continue;
                        }
                        ']' => {
                            // OSC: runs to BEL or ST (ESC \).
                            let mut j = i + 2;
                            while j < b.len() {
                                if b[j] == '\x07' {
                                    j += 1;
                                    break;
                                }
                                if b[j] == '\x1b' && j + 1 < b.len() && b[j + 1] == '\\' {
                                    j += 2;
                                    break;
                                }
                                j += 1;
                            }
                            if j >= b.len() && !b[i..].contains(&'\x07') {
                                self.pending = b[i..].iter().collect();
                                break;
                            }
                            i = j;
                            continue;
                        }
                        _ => {
                            i += 2; // two-character escape; ignore
                            continue;
                        }
                    }
                }
                '\n' => {
                    self.newline();
                    self.col = 0;
                }
                '\r' => self.col = 0,
                '\t' => {
                    let next = ((self.col / 8) + 1) * 8;
                    while self.col < next.min(self.cols) {
                        self.put(' ');
                    }
                }
                '\x08' => self.col = self.col.saturating_sub(1),
                c if (c as u32) < 0x20 || c as u32 == 0x7F => {}
                c => self.put(c),
            }
            i += 1;
        }

        if self.grid_hash() != before {
            self.revision += 1;
        }
    }

    /// Cheap change detector — not a cryptographic hash, just enough to notice a
    /// repaint is unnecessary.
    fn grid_hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for c in &self.grid {
            let v = c.ch as u64
                ^ ((c.ink as u64) << 24)
                ^ ((c.bold as u64) << 32)
                ^ ((c.dim as u64) << 33);
            h = (h ^ v).wrapping_mul(0x100_0000_01b3);
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_scroll_once_the_grid_is_full() {
        let mut t = Term::new(4, 2);
        t.feed("ab\ncd\nef");
        // Row 0 should now hold what was row 1.
        assert_eq!(t.cell(0, 0).ch, 'c');
        assert_eq!(t.cell(1, 0).ch, 'e');
    }

    #[test]
    fn sgr_sets_and_resets_semantic_ink() {
        let mut t = Term::new(8, 1);
        t.feed("\x1b[31mX\x1b[0mY");
        assert_eq!(t.cell(0, 0).ink, Ink::Cut);
        assert_eq!(t.cell(0, 1).ink, Ink::Body);
    }

    #[test]
    fn bold_and_dim_are_independent_of_colour() {
        let mut t = Term::new(8, 1);
        t.feed("\x1b[1m\x1b[2m\x1b[34mZ");
        let c = t.cell(0, 0);
        assert!(c.bold && c.dim);
        assert_eq!(c.ink, Ink::Wire);
    }

    #[test]
    fn a_sequence_split_across_chunks_still_applies() {
        let mut t = Term::new(8, 1);
        t.feed("\x1b[3");
        t.feed("2mQ");
        assert_eq!(t.cell(0, 0).ink, Ink::Sound);
        assert_eq!(t.cell(0, 0).ch, 'Q');
    }

    #[test]
    fn unhandled_csi_is_skipped_not_printed() {
        let mut t = Term::new(8, 1);
        t.feed("\x1b[2JA");
        assert_eq!(t.cell(0, 0).ch, 'A');
    }

    #[test]
    fn revision_only_advances_on_a_visible_change() {
        let mut t = Term::new(8, 1);
        t.feed("A");
        let r = t.revision;
        t.feed("\x1b[0m"); // attribute-only, nothing drawn
        assert_eq!(t.revision, r);
    }

    #[test]
    fn wrapping_moves_to_the_next_row() {
        let mut t = Term::new(2, 2);
        t.feed("abc");
        assert_eq!(t.cell(0, 0).ch, 'a');
        assert_eq!(t.cell(0, 1).ch, 'b');
        assert_eq!(t.cell(1, 0).ch, 'c');
    }
}
