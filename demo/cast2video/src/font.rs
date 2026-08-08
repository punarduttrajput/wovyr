//! A minimal TrueType renderer, written from scratch.
//!
//! Not a general font library — it does exactly what rasterising a terminal
//! recording needs, and nothing else:
//!
//! * `glyf`/`loca` outlines (simple **and** composite glyphs)
//! * `cmap` formats 4, 6 and 12, so box-drawing and dingbats resolve
//! * `head`/`hhea`/`hmtx` metrics
//! * a scanline rasteriser with nonzero winding and 4× vertical supersampling
//!   plus analytic horizontal coverage, which is what makes 15px text legible
//!
//! Deliberately absent: hinting, kerning, `CFF `/OpenType outlines, variable-font
//! axes (`fvar`/`gvar`), colour tables, bidi, shaping. A TTF whose outlines live
//! in `CFF ` is rejected up front with a clear message rather than rendering
//! blank glyphs.
//!
//! Why hand-rolled: this host pins `[net] offline = true` and no font crate
//! (`fontdue`, `ab_glyph`, `ttf-parser`, `swash`) is in the local registry cache,
//! so there is nothing to depend on. The upside is that the whole text pipeline
//! is inspectable and has no build requirements at all.

use std::collections::HashMap;

/// A point in font units.
type Point = (f32, f32);
/// A contour point: position plus whether it lies *on* the curve. An off-curve
/// point is a quadratic control point.
type CurvePoint = (f32, f32, bool);

/* ─────────────────────────────── byte reader ─────────────────────────────── */

struct Reader<'a> {
    d: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u8(&self, o: usize) -> Result<u8, String> {
        self.d.get(o).copied().ok_or_else(|| oob(o, self.d.len()))
    }
    fn u16(&self, o: usize) -> Result<u16, String> {
        if o + 2 > self.d.len() {
            return Err(oob(o, self.d.len()));
        }
        Ok(u16::from_be_bytes([self.d[o], self.d[o + 1]]))
    }
    fn i16(&self, o: usize) -> Result<i16, String> {
        self.u16(o).map(|v| v as i16)
    }
    fn u32(&self, o: usize) -> Result<u32, String> {
        if o + 4 > self.d.len() {
            return Err(oob(o, self.d.len()));
        }
        Ok(u32::from_be_bytes([
            self.d[o],
            self.d[o + 1],
            self.d[o + 2],
            self.d[o + 3],
        ]))
    }
}

fn oob(at: usize, len: usize) -> String {
    format!("font truncated: read at {at} past end {len}")
}

/* ──────────────────────────────── the face ───────────────────────────────── */

/// One rasterised glyph: an 8-bit coverage mask plus where to put it relative to
/// the pen position on the baseline.
pub struct Glyph {
    pub w: usize,
    pub h: usize,
    /// Offset from the pen x, in pixels (may be negative for overhang).
    pub left: i32,
    /// Offset from the baseline y, in pixels; negative is above the baseline.
    pub top: i32,
    pub cover: Vec<u8>,
}

impl Glyph {
    fn blank() -> Self {
        Glyph {
            w: 0,
            h: 0,
            left: 0,
            top: 0,
            cover: Vec::new(),
        }
    }
}

pub struct Face {
    data: Vec<u8>,
    tables: HashMap<[u8; 4], (usize, usize)>,
    units_per_em: f32,
    index_to_loc_long: bool,
    num_glyphs: u16,
    pub ascender: f32,
    pub descender: f32,
    /// Advance width of `M`, in font units. Every glyph in a monospace face
    /// shares it; used to derive the cell width.
    advance: f32,
    cmap: CmapIndex,
    /// Rasterised-glyph cache, keyed by `(char, px_size_in_64ths, synthetic_bold)`.
    cache: HashMap<(char, u32, bool), Glyph>,
}

enum CmapIndex {
    Fmt4 { off: usize },
    Fmt6 { off: usize },
    Fmt12 { off: usize },
}

impl Face {
    pub fn load(data: Vec<u8>) -> Result<Face, String> {
        let r = Reader { d: &data };
        let mut base = 0usize;

        // A .ttc collection starts with 'ttcf'; take its first face.
        if r.u32(0)? == 0x7474_6366 {
            base = r.u32(12)? as usize;
        }
        let ver = r.u32(base)?;
        if ver == 0x4F54_544F {
            return Err(
                "this font stores outlines in `CFF ` (OpenType/PostScript); \
                        cast2video only reads TrueType `glyf` outlines. Pick a .ttf \
                        such as CascadiaMono.ttf or consola.ttf."
                    .into(),
            );
        }
        if ver != 0x0001_0000 && ver != 0x7472_7565 {
            return Err(format!("unrecognised sfnt version {ver:#010x}"));
        }

        let num_tables = r.u16(base + 4)? as usize;
        let mut tables = HashMap::new();
        for i in 0..num_tables {
            let rec = base + 12 + i * 16;
            let tag = [r.u8(rec)?, r.u8(rec + 1)?, r.u8(rec + 2)?, r.u8(rec + 3)?];
            let off = r.u32(rec + 8)? as usize;
            let len = r.u32(rec + 12)? as usize;
            if off <= data.len() {
                tables.insert(tag, (off, len.min(data.len() - off)));
            }
        }

        let need = |t: &[u8; 4],
                    tables: &HashMap<[u8; 4], (usize, usize)>|
         -> Result<(usize, usize), String> {
            tables
                .get(t)
                .copied()
                .ok_or_else(|| format!("font has no `{}` table", String::from_utf8_lossy(t)))
        };

        let (head, _) = need(b"head", &tables)?;
        let units_per_em = r.u16(head + 18)? as f32;
        let index_to_loc_long = r.i16(head + 50)? == 1;

        let (maxp, _) = need(b"maxp", &tables)?;
        let num_glyphs = r.u16(maxp + 4)?;

        let (hhea, _) = need(b"hhea", &tables)?;
        let ascender = r.i16(hhea + 4)? as f32;
        let descender = r.i16(hhea + 6)? as f32;
        let num_hmetrics = r.u16(hhea + 34)? as usize;

        need(b"glyf", &tables)?;
        need(b"loca", &tables)?;

        let (hmtx, _) = need(b"hmtx", &tables)?;
        // Every advance in a monospace face is equal; read the first as the cell.
        let advance = if num_hmetrics > 0 {
            r.u16(hmtx)? as f32
        } else {
            units_per_em * 0.6
        };

        let (cmap_off, _) = need(b"cmap", &tables)?;
        let cmap = pick_cmap(&r, cmap_off)?;

        Ok(Face {
            data,
            tables,
            units_per_em,
            index_to_loc_long,
            num_glyphs,
            ascender,
            descender,
            advance,
            cmap,
            cache: HashMap::new(),
        })
    }

    fn r(&self) -> Reader<'_> {
        Reader { d: &self.data }
    }

    /// Cell advance in pixels at the given size.
    pub fn advance_px(&self, px: f32) -> f32 {
        self.advance * px / self.units_per_em
    }

    pub fn ascender_px(&self, px: f32) -> f32 {
        self.ascender * px / self.units_per_em
    }

    pub fn descender_px(&self, px: f32) -> f32 {
        self.descender * px / self.units_per_em
    }

    pub fn has(&mut self, c: char) -> bool {
        self.glyph_index(c).unwrap_or(0) != 0
    }

    fn glyph_index(&self, c: char) -> Result<u16, String> {
        let r = self.r();
        let cp = c as u32;
        match self.cmap {
            CmapIndex::Fmt4 { off } => {
                if cp > 0xFFFF {
                    return Ok(0);
                }
                let cp = cp as u16;
                let seg_count = (r.u16(off + 6)? / 2) as usize;
                let ends = off + 14;
                let starts = ends + seg_count * 2 + 2;
                let deltas = starts + seg_count * 2;
                let ranges = deltas + seg_count * 2;
                for s in 0..seg_count {
                    if r.u16(ends + s * 2)? >= cp {
                        let start = r.u16(starts + s * 2)?;
                        if start > cp {
                            return Ok(0);
                        }
                        let delta = r.u16(deltas + s * 2)?;
                        let ro = r.u16(ranges + s * 2)?;
                        if ro == 0 {
                            return Ok(cp.wrapping_add(delta));
                        }
                        let at = ranges + s * 2 + ro as usize + 2 * (cp - start) as usize;
                        let g = r.u16(at)?;
                        return Ok(if g == 0 { 0 } else { g.wrapping_add(delta) });
                    }
                }
                Ok(0)
            }
            CmapIndex::Fmt6 { off } => {
                let first = r.u16(off + 6)? as u32;
                let count = r.u16(off + 8)? as u32;
                if cp < first || cp >= first + count {
                    return Ok(0);
                }
                r.u16(off + 10 + 2 * (cp - first) as usize)
            }
            CmapIndex::Fmt12 { off } => {
                let n = r.u32(off + 12)? as usize;
                for g in 0..n {
                    let rec = off + 16 + g * 12;
                    let start = r.u32(rec)?;
                    let end = r.u32(rec + 4)?;
                    if cp >= start && cp <= end {
                        return Ok((r.u32(rec + 8)? + (cp - start)) as u16);
                    }
                    if start > cp {
                        break;
                    }
                }
                Ok(0)
            }
        }
    }

    fn loca(&self, gid: u16) -> Result<(usize, usize), String> {
        let (loca, _) = self.tables[b"loca"];
        let (glyf, glyf_len) = self.tables[b"glyf"];
        let r = self.r();
        let (a, b) = if self.index_to_loc_long {
            (
                r.u32(loca + gid as usize * 4)? as usize,
                r.u32(loca + gid as usize * 4 + 4)? as usize,
            )
        } else {
            (
                r.u16(loca + gid as usize * 2)? as usize * 2,
                r.u16(loca + gid as usize * 2 + 2)? as usize * 2,
            )
        };
        if b <= a || b > glyf_len {
            return Ok((glyf + a, 0)); // empty glyph (e.g. space)
        }
        Ok((glyf + a, b - a))
    }

    /// Collect a glyph's contours as closed polylines in font units, applying
    /// `xform` (a 2×2 plus translation) so composite components land correctly.
    fn outline(
        &self,
        gid: u16,
        xform: [f32; 6],
        depth: u8,
        out: &mut Vec<Vec<Point>>,
    ) -> Result<(), String> {
        if depth > 5 || gid >= self.num_glyphs {
            return Ok(());
        }
        let (off, len) = self.loca(gid)?;
        if len == 0 {
            return Ok(());
        }
        let r = self.r();
        let n_contours = r.i16(off)?;

        if n_contours < 0 {
            // Composite: walk components, each with its own transform.
            let mut p = off + 10;
            loop {
                let flags = r.u16(p)?;
                let sub = r.u16(p + 2)?;
                p += 4;
                let (dx, dy) = if flags & 1 != 0 {
                    let a = (r.i16(p)? as f32, r.i16(p + 2)? as f32);
                    p += 4;
                    a
                } else {
                    let a = (r.u8(p)? as i8 as f32, r.u8(p + 1)? as i8 as f32);
                    p += 2;
                    a
                };
                let f2 = |v: i16| v as f32 / 16384.0;
                let m = if flags & 0x08 != 0 {
                    let s = f2(r.i16(p)?);
                    p += 2;
                    [s, 0.0, 0.0, s]
                } else if flags & 0x40 != 0 {
                    let sx = f2(r.i16(p)?);
                    let sy = f2(r.i16(p + 2)?);
                    p += 4;
                    [sx, 0.0, 0.0, sy]
                } else if flags & 0x80 != 0 {
                    let m = [
                        f2(r.i16(p)?),
                        f2(r.i16(p + 2)?),
                        f2(r.i16(p + 4)?),
                        f2(r.i16(p + 6)?),
                    ];
                    p += 8;
                    m
                } else {
                    [1.0, 0.0, 0.0, 1.0]
                };
                // Compose child transform with the parent's.
                let c = [
                    m[0] * xform[0] + m[1] * xform[2],
                    m[0] * xform[1] + m[1] * xform[3],
                    m[2] * xform[0] + m[3] * xform[2],
                    m[2] * xform[1] + m[3] * xform[3],
                    dx * xform[0] + dy * xform[2] + xform[4],
                    dx * xform[1] + dy * xform[3] + xform[5],
                ];
                self.outline(sub, c, depth + 1, out)?;
                if flags & 0x20 == 0 {
                    break;
                }
            }
            return Ok(());
        }

        // Simple glyph.
        let nc = n_contours as usize;
        let mut ends = Vec::with_capacity(nc);
        for i in 0..nc {
            ends.push(r.u16(off + 10 + i * 2)? as usize);
        }
        let n_pts = ends.last().map(|e| e + 1).unwrap_or(0);
        let instr_len = r.u16(off + 10 + nc * 2)? as usize;
        let mut p = off + 10 + nc * 2 + 2 + instr_len;

        let mut flags = Vec::with_capacity(n_pts);
        while flags.len() < n_pts {
            let f = r.u8(p)?;
            p += 1;
            flags.push(f);
            if f & 0x08 != 0 {
                let rep = r.u8(p)?;
                p += 1;
                for _ in 0..rep {
                    if flags.len() < n_pts {
                        flags.push(f);
                    }
                }
            }
        }

        let mut xs = Vec::with_capacity(n_pts);
        let mut v = 0i32;
        for &f in &flags {
            if f & 0x02 != 0 {
                let d = r.u8(p)? as i32;
                p += 1;
                v += if f & 0x10 != 0 { d } else { -d };
            } else if f & 0x10 == 0 {
                v += r.i16(p)? as i32;
                p += 2;
            }
            xs.push(v as f32);
        }
        let mut ys = Vec::with_capacity(n_pts);
        v = 0;
        for &f in &flags {
            if f & 0x04 != 0 {
                let d = r.u8(p)? as i32;
                p += 1;
                v += if f & 0x20 != 0 { d } else { -d };
            } else if f & 0x20 == 0 {
                v += r.i16(p)? as i32;
                p += 2;
            }
            ys.push(v as f32);
        }

        let tx = |x: f32, y: f32| {
            (
                x * xform[0] + y * xform[2] + xform[4],
                x * xform[1] + y * xform[3] + xform[5],
            )
        };

        let mut start = 0usize;
        for &end in &ends {
            if end >= n_pts {
                break;
            }
            let pts: Vec<CurvePoint> = (start..=end)
                .map(|i| {
                    let (x, y) = tx(xs[i], ys[i]);
                    (x, y, flags[i] & 0x01 != 0)
                })
                .collect();
            start = end + 1;
            if pts.len() < 2 {
                continue;
            }
            out.push(flatten_contour(&pts));
        }
        Ok(())
    }

    /// Rasterise `c` at `px`, memoised. `bold` fakes weight by smearing the
    /// coverage horizontally — Cascadia and Consolas ship weight as separate
    /// files or a variable axis, and this reader supports neither.
    pub fn glyph(&mut self, c: char, px: f32, bold: bool) -> &Glyph {
        let key = (c, (px * 64.0) as u32, bold);
        if !self.cache.contains_key(&key) {
            let g = self.render(c, px, bold).unwrap_or_else(|_| Glyph::blank());
            self.cache.insert(key, g);
        }
        &self.cache[&key]
    }

    fn render(&self, c: char, px: f32, bold: bool) -> Result<Glyph, String> {
        let gid = self.glyph_index(c)?;
        let mut contours = Vec::new();
        self.outline(gid, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0, &mut contours)?;
        if contours.is_empty() {
            return Ok(Glyph::blank());
        }
        let scale = px / self.units_per_em;

        // Device-space bounds, y flipped (font is y-up, raster is y-down).
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for ct in &contours {
            for &(x, y) in ct {
                x0 = x0.min(x * scale);
                x1 = x1.max(x * scale);
                y0 = y0.min(-y * scale);
                y1 = y1.max(-y * scale);
            }
        }
        let smear = if bold { (px * 0.055).max(0.6) } else { 0.0 };
        let left = x0.floor() as i32 - 1;
        let top = y0.floor() as i32 - 1;
        let w = ((x1 + smear).ceil() as i32 - left + 1).max(1) as usize;
        let h = (y1.ceil() as i32 - top + 1).max(1) as usize;
        if w > 4096 || h > 4096 {
            return Ok(Glyph::blank());
        }

        // Edges in device space.
        let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
        for ct in &contours {
            for i in 0..ct.len() {
                let a = ct[i];
                let b = ct[(i + 1) % ct.len()];
                let (ax, ay) = (a.0 * scale - left as f32, -a.1 * scale - top as f32);
                let (bx, by) = (b.0 * scale - left as f32, -b.1 * scale - top as f32);
                if (ay - by).abs() > 1e-9 {
                    edges.push((ax, ay, bx, by));
                }
            }
        }

        const SS: usize = 4; // vertical subsamples per pixel row
        let mut cover = vec![0f32; w * h];
        let mut xs: Vec<(f32, i32)> = Vec::with_capacity(16);

        for row in 0..h {
            for s in 0..SS {
                let sy = row as f32 + (s as f32 + 0.5) / SS as f32;
                xs.clear();
                for &(ax, ay, bx, by) in &edges {
                    let (lo, hi) = if ay < by { (ay, by) } else { (by, ay) };
                    if sy < lo || sy >= hi {
                        continue;
                    }
                    let t = (sy - ay) / (by - ay);
                    xs.push((ax + t * (bx - ax), if by > ay { 1 } else { -1 }));
                }
                if xs.is_empty() {
                    continue;
                }
                xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                // Nonzero winding: accumulate direction, fill while != 0.
                let mut wind = 0;
                for i in 0..xs.len().saturating_sub(1) {
                    wind += xs[i].1;
                    if wind != 0 {
                        span(
                            &mut cover[row * w..(row + 1) * w],
                            xs[i].0,
                            xs[i + 1].0 + smear,
                            1.0 / SS as f32,
                        );
                    }
                }
            }
        }

        let bytes = cover
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            .collect();
        Ok(Glyph {
            w,
            h,
            left,
            top,
            cover: bytes,
        })
    }
}

/// Add horizontal coverage for `[xa, xb)` on one sub-scanline, anti-aliasing the
/// partially-covered pixel at each end analytically rather than by supersampling.
fn span(row: &mut [f32], xa: f32, xb: f32, weight: f32) {
    let w = row.len() as f32;
    let a = xa.max(0.0);
    let b = xb.min(w);
    if b <= a {
        return;
    }
    let ia = a.floor() as usize;
    let ib = (b.ceil() as usize).min(row.len());
    for (px, cell) in row.iter_mut().enumerate().take(ib).skip(ia) {
        let l = (px as f32).max(a);
        let r = ((px + 1) as f32).min(b);
        if r > l {
            *cell += (r - l) * weight;
        }
    }
}

/// Turn a TrueType contour (on- and off-curve points) into a closed polyline,
/// inserting the implied on-curve midpoint between consecutive control points.
fn flatten_contour(pts: &[CurvePoint]) -> Vec<Point> {
    let n = pts.len();
    // Start from an on-curve point; if the contour has none, synthesise one.
    let start = pts.iter().position(|p| p.2);
    let mut poly: Vec<Point> = Vec::with_capacity(n * 4);
    let mid = |a: CurvePoint, b: CurvePoint| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);

    let (first, order): (Point, Vec<CurvePoint>) = match start {
        Some(s) => (
            (pts[s].0, pts[s].1),
            (1..=n).map(|k| pts[(s + k) % n]).collect(),
        ),
        None => (mid(pts[n - 1], pts[0]), pts.to_vec()),
    };
    poly.push(first);
    let mut cur = first;
    let mut ctrl: Option<Point> = None;

    for p in order {
        if p.2 {
            match ctrl.take() {
                None => poly.push((p.0, p.1)),
                Some(c) => {
                    quad(&mut poly, cur, c, (p.0, p.1));
                }
            }
            cur = *poly.last().unwrap();
        } else if let Some(c) = ctrl {
            let m = ((c.0 + p.0) / 2.0, (c.1 + p.1) / 2.0);
            quad(&mut poly, cur, c, m);
            cur = m;
            ctrl = Some((p.0, p.1));
        } else {
            ctrl = Some((p.0, p.1));
        }
    }
    if let Some(c) = ctrl {
        quad(&mut poly, cur, c, first);
    }
    poly
}

/// Flatten one quadratic Bézier, stepping proportionally to its extent so small
/// curves do not pay for segments they cannot show.
fn quad(out: &mut Vec<Point>, p0: Point, c: Point, p1: Point) {
    let d = (p0.0 - c.0).abs() + (p0.1 - c.1).abs() + (c.0 - p1.0).abs() + (c.1 - p1.1).abs();
    let steps = ((d / 40.0).sqrt().ceil() as usize).clamp(2, 16);
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let u = 1.0 - t;
        out.push((
            u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0,
            u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1,
        ));
    }
}

/// Prefer a full-Unicode subtable, then BMP, then anything Windows or Mac.
fn pick_cmap(r: &Reader<'_>, cmap: usize) -> Result<CmapIndex, String> {
    let n = r.u16(cmap + 2)? as usize;
    let mut best: Option<(u8, usize)> = None;
    for i in 0..n {
        let rec = cmap + 4 + i * 8;
        let plat = r.u16(rec)?;
        let enc = r.u16(rec + 2)?;
        let off = cmap + r.u32(rec + 4)? as usize;
        let fmt = r.u16(off)?;
        let rank = match (plat, enc, fmt) {
            (3, 10, 12) => 5,
            (0, 4..=6, 12) => 5,
            (3, 1, 4) => 4,
            (0, 3, 4) => 4,
            (0, _, 4) => 3,
            (3, 0, 4) => 2,
            (_, _, 6) => 1,
            _ => 0,
        };
        if rank > 0 && best.map(|(b, _)| rank > b).unwrap_or(true) {
            best = Some((rank, off));
        }
    }
    let (_, off) = best.ok_or("font has no usable cmap subtable (need format 4, 6 or 12)")?;
    Ok(match r.u16(off)? {
        12 => CmapIndex::Fmt12 { off },
        6 => CmapIndex::Fmt6 { off },
        _ => CmapIndex::Fmt4 { off },
    })
}
