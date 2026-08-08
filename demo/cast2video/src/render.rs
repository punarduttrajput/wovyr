//! Paint a [`Term`] grid into an RGB24 frame.
//!
//! Holds a primary face plus optional fallbacks, because a monospace terminal
//! font routinely lacks the dingbats a narrated recording uses — CascadiaMono
//! has no `✗` (U+2717), which in this project's palette is the glyph that marks
//! every denial. Falling back is what real terminals do; the alternative was
//! shipping a video whose most important mark is a blank cell.

use std::collections::HashMap;

use crate::font::Face;
use crate::term::Term;
use crate::theme::Theme;

pub struct Renderer {
    pub px: f32,
    pub cell_w: usize,
    pub line_h: usize,
    pub pad: usize,
    pub width: usize,
    pub height: usize,
    baseline: i32,
    theme: &'static Theme,
    /// Index 0 is the primary face and defines the cell metrics; the rest are
    /// consulted only for characters it cannot draw.
    faces: Vec<Face>,
    /// Which face serves each character, decided once.
    route: HashMap<char, Option<usize>>,
}

impl Renderer {
    pub fn new(
        faces: Vec<Face>,
        px: f32,
        line_height: f32,
        pad: usize,
        cols: usize,
        rows: usize,
        theme: &'static Theme,
    ) -> Renderer {
        let primary = &faces[0];
        // A monospace cell must be a whole number of pixels, or columns drift
        // apart across 90 of them; round once here and use it everywhere.
        let cell_w = primary.advance_px(px).round().max(1.0) as usize;
        let line_h = (px * line_height).round().max(1.0) as usize;

        // Centre the ink box vertically inside the line box.
        let asc = primary.ascender_px(px);
        let desc = primary.descender_px(px); // negative
        let leading = (line_h as f32 - (asc - desc)) / 2.0;
        let baseline = (leading + asc).round() as i32;

        // h264 requires even dimensions; pad rather than crop so nothing is lost.
        let mut width = pad * 2 + cols * cell_w;
        let mut height = pad * 2 + rows * line_h;
        width += width % 2;
        height += height % 2;

        Renderer {
            px,
            cell_w,
            line_h,
            pad,
            width,
            height,
            baseline,
            theme,
            faces,
            route: HashMap::new(),
        }
    }

    pub fn theme(&self) -> &'static Theme {
        self.theme
    }

    /// Which face will draw `c`, or `None` if no loaded face can.
    fn route_of(&mut self, c: char) -> Option<usize> {
        if let Some(&r) = self.route.get(&c) {
            return r;
        }
        let mut found = None;
        for i in 0..self.faces.len() {
            if self.faces[i].has(c) {
                found = Some(i);
                break;
            }
        }
        self.route.insert(c, found);
        found
    }

    /// Characters no loaded face can draw. Reported rather than silently blank.
    pub fn missing(&mut self, chars: impl Iterator<Item = char>) -> Vec<char> {
        let mut miss = Vec::new();
        for c in chars {
            if c == ' ' {
                continue;
            }
            if self.route_of(c).is_none() && !miss.contains(&c) {
                miss.push(c);
            }
        }
        miss
    }

    /// Which characters are served by a fallback rather than the primary face.
    pub fn fallbacks_used(&mut self, chars: impl Iterator<Item = char>) -> Vec<(char, usize)> {
        let mut used: Vec<(char, usize)> = Vec::new();
        for c in chars {
            if c == ' ' {
                continue;
            }
            if let Some(i) = self.route_of(c) {
                if i > 0 && !used.iter().any(|(u, _)| *u == c) {
                    used.push((c, i));
                }
            }
        }
        used
    }

    pub fn frame(&mut self, term: &Term, buf: &mut Vec<u8>) {
        // Copy every scalar out first: the paint loop borrows `self.faces`
        // mutably (glyph rasterising memoises), so it cannot also read fields
        // through `self`.
        let (w, h) = (self.width, self.height);
        let (cell_w, line_h, pad, px, baseline) =
            (self.cell_w, self.line_h, self.pad, self.px, self.baseline);
        let theme = self.theme;

        buf.clear();
        buf.resize(w * h * 3, 0);
        for px3 in buf.chunks_exact_mut(3) {
            px3.copy_from_slice(&theme.bg);
        }

        for r in 0..term.rows {
            for c in 0..term.cols {
                let cell = term.cell(r, c);
                if cell.ch == ' ' {
                    continue;
                }
                let Some(fi) = self.route_of(cell.ch) else {
                    continue;
                };
                let fg = theme.resolve(cell.ink, cell.bold, cell.dim);
                let pen_x = (pad + c * cell_w) as i32;
                let base_y = (pad + r * line_h) as i32 + baseline;

                let g = self.faces[fi].glyph(cell.ch, px, cell.bold);
                if g.w == 0 {
                    continue;
                }
                // A fallback face is usually proportional, so its glyph has no
                // reason to sit on this cell's advance. Centre it instead of
                // letting it collide with the neighbouring column.
                let x_off = if fi == 0 {
                    g.left
                } else {
                    (cell_w as i32 - g.w as i32) / 2
                };

                for gy in 0..g.h {
                    let y = base_y + g.top + gy as i32;
                    if y < 0 || y as usize >= h {
                        continue;
                    }
                    let row = y as usize * w;
                    for gx in 0..g.w {
                        let a = g.cover[gy * g.w + gx] as u32;
                        if a == 0 {
                            continue;
                        }
                        let x = pen_x + x_off + gx as i32;
                        if x < 0 || x as usize >= w {
                            continue;
                        }
                        let i = (row + x as usize) * 3;
                        for k in 0..3 {
                            let d = buf[i + k] as u32;
                            buf[i + k] = ((d * (255 - a) + fg[k] as u32 * a) / 255) as u8;
                        }
                    }
                }
            }
        }
    }
}
