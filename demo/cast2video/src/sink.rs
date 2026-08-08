//! Where frames go: an MP4 via ffmpeg, a GIF, or a PNG sequence.
//!
//! Only the MP4 path needs an external program. Encoding H.264 in-process would
//! mean either vendoring an encoder or depending on one that is not in this
//! host's registry cache, so the tool pipes raw frames to ffmpeg — the same
//! shell-out-to-a-platform-tool pattern `wovyr-tools` already uses for
//! `iptables`/`nsenter` and `icacls`. GIF and PNG are pure Rust, so the tool is
//! never *dependent* on ffmpeg being present.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::theme::{Rgb, Theme, mix};

pub enum Sink {
    Mp4 {
        child: Child,
    },
    Gif {
        enc: gif::Encoder<BufWriter<File>>,
        pal: Palette,
        delay: u16,
    },
    Png {
        dir: PathBuf,
        n: usize,
    },
}

impl Sink {
    pub fn mp4(path: &Path, w: usize, h: usize, fps: u32, crf: u32) -> Result<Sink, String> {
        let child = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-s",
                &format!("{w}x{h}"),
                "-r",
                &fps.to_string(),
                "-i",
                "-",
                "-c:v",
                "libx264",
                "-preset",
                "slow",
                "-crf",
                &crf.to_string(),
                // Chroma subsampling + baseline-friendly flags, so the file plays
                // in a browser and in Keynote/Slides, not just in VLC.
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "could not start ffmpeg ({e}). Install it, or write a GIF or PNG \
                     sequence instead — those need no external program."
                )
            })?;
        Ok(Sink::Mp4 { child })
    }

    pub fn gif(path: &Path, w: usize, h: usize, fps: u32, theme: &Theme) -> Result<Sink, String> {
        let pal = Palette::for_theme(theme);
        let f = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut enc = gif::Encoder::new(BufWriter::new(f), w as u16, h as u16, &pal.flat)
            .map_err(|e| format!("gif: {e}"))?;
        enc.set_repeat(gif::Repeat::Infinite)
            .map_err(|e| format!("gif: {e}"))?;
        // GIF delays are in hundredths of a second, so only a few frame rates are
        // exactly representable; 20fps (delay 5) and 10fps (delay 10) are clean.
        let delay = ((100.0 / fps as f64).round() as u16).max(2);
        Ok(Sink::Gif { enc, pal, delay })
    }

    pub fn png(dir: &Path) -> Result<Sink, String> {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Sink::Png {
            dir: dir.to_path_buf(),
            n: 0,
        })
    }

    pub fn push(&mut self, rgb: &[u8], w: usize, h: usize) -> Result<(), String> {
        match self {
            Sink::Mp4 { child } => child
                .stdin
                .as_mut()
                .ok_or("ffmpeg stdin closed")?
                .write_all(rgb)
                .map_err(|e| format!("writing to ffmpeg: {e}")),
            Sink::Gif { enc, pal, delay } => {
                let idx = pal.map(rgb);
                let mut frame = gif::Frame::from_indexed_pixels(w as u16, h as u16, idx, None);
                frame.delay = *delay;
                enc.write_frame(&frame).map_err(|e| format!("gif: {e}"))
            }
            Sink::Png { dir, n } => {
                let path = dir.join(format!("frame_{n:05}.png"));
                *n += 1;
                let f = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let mut e = png::Encoder::new(BufWriter::new(f), w as u32, h as u32);
                e.set_color(png::ColorType::Rgb);
                e.set_depth(png::BitDepth::Eight);
                e.write_header()
                    .and_then(|mut wr| wr.write_image_data(rgb))
                    .map_err(|e| format!("png: {e}"))
            }
        }
    }

    pub fn finish(self) -> Result<(), String> {
        match self {
            Sink::Mp4 { mut child } => {
                drop(child.stdin.take()); // EOF, so ffmpeg flushes and muxes
                let st = child
                    .wait()
                    .map_err(|e| format!("waiting for ffmpeg: {e}"))?;
                if !st.success() {
                    return Err(format!("ffmpeg exited with {st}"));
                }
                Ok(())
            }
            Sink::Gif { enc, .. } => {
                drop(enc);
                Ok(())
            }
            Sink::Png { .. } => Ok(()),
        }
    }
}

/// A GIF palette built **analytically** from the theme rather than quantised
/// per frame.
///
/// Every pixel in a frame is the background blended with exactly one ink (that
/// is all anti-aliased text produces), so enumerating those ramps up front gives
/// a near-exact palette — no dithering, no frame-to-frame palette flicker, and
/// no need for a quantiser at all.
pub struct Palette {
    pub flat: Vec<u8>,
    entries: Vec<Rgb>,
}

impl Palette {
    pub fn for_theme(t: &Theme) -> Palette {
        let inks = [t.body, t.strong, t.cut, t.indigo, t.indigo_soft, t.note];
        const STEPS: usize = 40;
        let mut entries = vec![t.bg];
        for ink in inks {
            for s in 1..=STEPS {
                let c = mix(t.bg, ink, s as f32 / STEPS as f32);
                if !entries.contains(&c) {
                    entries.push(c);
                }
            }
        }
        entries.truncate(256);
        let mut flat = Vec::with_capacity(256 * 3);
        for e in &entries {
            flat.extend_from_slice(e);
        }
        // GIF wants a power-of-two table; pad with the background.
        while flat.len() < 256 * 3 {
            flat.extend_from_slice(&t.bg);
        }
        Palette { flat, entries }
    }

    fn map(&self, rgb: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rgb.len() / 3);
        // Frames are mostly long runs of one colour, so remembering the last
        // match turns nearly every pixel into a single comparison.
        let mut last: Option<([u8; 3], u8)> = None;
        for px in rgb.chunks_exact(3) {
            let p = [px[0], px[1], px[2]];
            if let Some((c, i)) = last {
                if c == p {
                    out.push(i);
                    continue;
                }
            }
            let mut best = 0u8;
            let mut bd = u32::MAX;
            for (i, e) in self.entries.iter().enumerate() {
                let d = (e[0] as i32 - p[0] as i32).pow(2) as u32
                    + (e[1] as i32 - p[1] as i32).pow(2) as u32
                    + (e[2] as i32 - p[2] as i32).pow(2) as u32;
                if d < bd {
                    bd = d;
                    best = i as u8;
                    if d == 0 {
                        break;
                    }
                }
            }
            last = Some((p, best));
            out.push(best);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_reproduces_theme_colours_exactly() {
        let t = &Theme::DARK;
        let p = Palette::for_theme(t);
        // The background and each full-strength ink must round-trip losslessly,
        // otherwise madder would drift toward body text in the GIF.
        for c in [
            t.bg,
            t.body,
            t.strong,
            t.cut,
            t.indigo,
            t.indigo_soft,
            t.note,
        ] {
            let idx = p.map(&c)[0] as usize;
            assert_eq!(p.entries[idx], c, "palette lost {c:?}");
        }
    }

    #[test]
    fn palette_never_exceeds_the_gif_limit() {
        for t in [&Theme::DARK, &Theme::LIGHT] {
            let p = Palette::for_theme(t);
            assert!(p.entries.len() <= 256);
            assert_eq!(p.flat.len(), 768);
        }
    }
}
