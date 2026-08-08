//! `cast2video` — turn an asciinema cast into a real video file.
//!
//! The text pipeline is entirely self-contained: a from-scratch TrueType reader
//! and scanline rasteriser (`font.rs`), a small terminal model (`term.rs`), and a
//! painter (`render.rs`). GIF and PNG output are pure Rust. MP4 pipes raw frames
//! to ffmpeg, because encoding H.264 in-process would need a dependency this
//! host cannot fetch.
//!
//!   cast2video demo/out/demo.cast --out demo/out/demo.mp4
//!   cast2video demo/out/demo.cast --out demo/out/demo.gif --fps 20 --idle-cap 1.2
//!   cast2video demo/out/demo.cast --out demo/out/frames/

mod cast;
mod font;
mod render;
mod sink;
mod term;
mod theme;

use std::path::{Path, PathBuf};
use std::time::Instant;

use cast::Cast;
use font::Face;
use render::Renderer;
use sink::Sink;
use term::Term;
use theme::Theme;

const USAGE: &str = "\
cast2video — rasterise an asciinema cast into a video

USAGE:
    cast2video <cast> --out <path> [options]

    <path> ending .mp4/.mkv/.webm  encodes via ffmpeg
           ending .gif             writes an animated GIF (no ffmpeg needed)
           any other path          treated as a directory for a PNG sequence

OPTIONS:
    --font <path>        TrueType (.ttf/.ttc) font. Default: first monospace
                         face found for this platform.
    --fallback <path>    Face to consult for characters the primary lacks.
                         Repeatable, tried in order. Defaults to this
                         platform's symbol font, because monospace faces
                         routinely lack dingbats such as U+2717.
    --no-fallback        Use only the primary face.
    --probe              Report glyph coverage for the cast and exit without
                         rendering.
    --font-size <px>     Cell size in pixels (default 18).
    --line-height <f>    Multiple of font size (default 1.62, matching the
                         HTML player).
    --pad <px>           Border around the grid (default 24).
    --theme <name>       dark (default) or light.
    --fps <n>            Frames per second (default 30; 20 suits a GIF).
    --speed <f>          Divide all timings by this (default 1.0).
    --idle-cap <secs>    Clamp any pause longer than this.
    --hold <secs>        Freeze on the final frame (default 2.0).
    --crf <n>            x264 quality for MP4, lower is better (default 18).
    --cols / --rows <n>  Override the grid in the cast header.
    -h, --help           This text.
";

/// Fonts to try when `--font` is not given. Monospace faces with `glyf` outlines
/// and good box-drawing coverage, most-preferred first.
const FONT_CANDIDATES: &[&str] = &[
    // Windows
    r"C:\Windows\Fonts\CascadiaMono.ttf",
    r"C:\Windows\Fonts\CascadiaCode.ttf",
    r"C:\Windows\Fonts\consola.ttf",
    r"C:\Windows\Fonts\lucon.ttf",
    // macOS
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/SFNSMono.ttf",
    "/Library/Fonts/Courier New.ttf",
    // Linux
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
];

/// Faces consulted for characters the primary lacks. Broad symbol coverage
/// matters more than matching the terminal face here, since these only ever
/// draw the handful of dingbats a monospace font is missing.
const FALLBACK_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\DejaVuSans.ttf",
    r"C:\Windows\Fonts\arialuni.ttf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/unifont/unifont.ttf",
];

struct Args {
    cast: PathBuf,
    out: PathBuf,
    font: Option<PathBuf>,
    fallbacks: Vec<PathBuf>,
    no_fallback: bool,
    probe: bool,
    font_size: f32,
    line_height: f32,
    pad: usize,
    theme: &'static Theme,
    fps: u32,
    speed: f64,
    idle_cap: Option<f64>,
    hold: f64,
    crf: u32,
    cols: Option<usize>,
    rows: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut cast: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut a = Args {
        cast: PathBuf::new(),
        out: PathBuf::new(),
        font: None,
        fallbacks: Vec::new(),
        no_fallback: false,
        probe: false,
        font_size: 18.0,
        line_height: 1.62,
        pad: 24,
        theme: &Theme::DARK,
        fps: 30,
        speed: 1.0,
        idle_cap: None,
        hold: 2.0,
        crf: 18,
        cols: None,
        rows: None,
    };

    while let Some(arg) = it.next() {
        let mut val =
            || -> Result<String, String> { it.next().ok_or(format!("{arg} needs a value")) };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--out" => out = Some(PathBuf::from(val()?)),
            "--font" => a.font = Some(PathBuf::from(val()?)),
            "--fallback" => a.fallbacks.push(PathBuf::from(val()?)),
            "--no-fallback" => a.no_fallback = true,
            "--probe" => a.probe = true,
            "--font-size" => {
                a.font_size = val()?.parse().map_err(|_| "--font-size must be a number")?
            }
            "--line-height" => {
                a.line_height = val()?
                    .parse()
                    .map_err(|_| "--line-height must be a number")?
            }
            "--pad" => a.pad = val()?.parse().map_err(|_| "--pad must be an integer")?,
            "--fps" => a.fps = val()?.parse().map_err(|_| "--fps must be an integer")?,
            "--speed" => a.speed = val()?.parse().map_err(|_| "--speed must be a number")?,
            "--idle-cap" => {
                a.idle_cap = Some(val()?.parse().map_err(|_| "--idle-cap must be a number")?)
            }
            "--hold" => a.hold = val()?.parse().map_err(|_| "--hold must be a number")?,
            "--crf" => a.crf = val()?.parse().map_err(|_| "--crf must be an integer")?,
            "--cols" => a.cols = Some(val()?.parse().map_err(|_| "--cols must be an integer")?),
            "--rows" => a.rows = Some(val()?.parse().map_err(|_| "--rows must be an integer")?),
            "--theme" => {
                let n = val()?;
                a.theme =
                    Theme::by_name(&n).ok_or(format!("unknown theme `{n}` (dark or light)"))?;
            }
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
            s => {
                if cast.is_some() {
                    return Err(format!("unexpected extra argument `{s}`"));
                }
                cast = Some(PathBuf::from(s));
            }
        }
    }

    a.cast = cast.ok_or("no cast file given (try --help)")?;
    // --probe never writes anything, so it does not need a destination.
    a.out = match out {
        Some(o) => o,
        None if a.probe => PathBuf::new(),
        None => return Err("--out is required".into()),
    };
    if a.fps == 0 {
        return Err("--fps must be at least 1".into());
    }
    if a.speed <= 0.0 {
        return Err("--speed must be positive".into());
    }
    if a.font_size < 4.0 {
        return Err("--font-size must be at least 4".into());
    }
    Ok(a)
}

fn find_font(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        if !p.exists() {
            return Err(format!("font not found: {}", p.display()));
        }
        return Ok(p.to_path_buf());
    }
    for c in FONT_CANDIDATES {
        let p = Path::new(c);
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }
    Err(format!(
        "no monospace font found automatically. Pass --font <path-to.ttf>.\nTried:\n  {}",
        FONT_CANDIDATES.join("\n  ")
    ))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("cast2video: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let a = parse_args()?;

    let src = std::fs::read_to_string(&a.cast).map_err(|e| format!("{}: {e}", a.cast.display()))?;
    let c = Cast::parse(&src)?;
    let cols = a.cols.unwrap_or(c.cols);
    let rows = a.rows.unwrap_or(c.rows);

    let font_path = find_font(a.font.as_deref())?;
    let mut face_paths = vec![font_path.clone()];
    if !a.no_fallback {
        if a.fallbacks.is_empty() {
            // Take only the first available default: one symbol face is enough,
            // and each one loaded costs memory and a slower miss path.
            if let Some(p) = FALLBACK_CANDIDATES
                .iter()
                .map(Path::new)
                .find(|p| p.exists())
            {
                face_paths.push(p.to_path_buf());
            }
        } else {
            for p in &a.fallbacks {
                if !p.exists() {
                    return Err(format!("fallback font not found: {}", p.display()));
                }
                face_paths.push(p.clone());
            }
        }
    }

    let mut faces = Vec::with_capacity(face_paths.len());
    for p in &face_paths {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        faces.push(Face::load(bytes).map_err(|e| format!("{}: {e}", p.display()))?);
    }
    let names: Vec<String> = face_paths
        .iter()
        .map(|p| {
            p.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let mut r = Renderer::new(
        faces,
        a.font_size,
        a.line_height,
        a.pad,
        cols,
        rows,
        a.theme,
    );

    // Glyph coverage: report what falls back and what cannot be drawn at all,
    // rather than shipping a video with blank cells where a mark should be.
    let all: String = c.events.iter().map(|e| e.data.as_str()).collect();
    let chars: Vec<char> = all.chars().filter(|ch| !ch.is_control()).collect();

    let used = r.fallbacks_used(chars.iter().copied());
    for (ch, fi) in &used {
        eprintln!(
            "cast2video: U+{:04X} `{ch}` not in {}, drawn from {}",
            *ch as u32, names[0], names[*fi]
        );
    }
    let missing = r.missing(chars.iter().copied());
    if !missing.is_empty() {
        let show: String = missing.iter().take(12).collect();
        eprintln!(
            "cast2video: warning — no loaded face has {} character(s): {show}\n\
             \x20            they will render blank; pass --fallback <path-to.ttf>.",
            missing.len()
        );
    }

    if a.probe {
        let distinct = {
            let mut d: Vec<char> = chars.clone();
            d.sort_unstable();
            d.dedup();
            d
        };
        eprintln!(
            "cast2video: probe — {} distinct characters, {} from the primary face, \
             {} from a fallback, {} missing",
            distinct.len(),
            distinct.len() - used.len() - missing.len(),
            used.len(),
            missing.len(),
        );
        return Ok(());
    }

    let times = c.retime(a.idle_cap, a.speed);
    let span = times.last().copied().unwrap_or(0.0);
    let total = ((span + a.hold) * a.fps as f64).ceil() as usize + 1;

    let ext = a
        .out
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut sink = match ext.as_str() {
        "mp4" | "mkv" | "webm" | "mov" => Sink::mp4(&a.out, r.width, r.height, a.fps, a.crf)?,
        "gif" => Sink::gif(&a.out, r.width, r.height, a.fps, r.theme())?,
        _ => Sink::png(&a.out)?,
    };

    if let Some(t) = &c.title {
        eprintln!("cast2video: \"{t}\"");
    }
    eprintln!(
        "cast2video: {}x{} · {cols}x{rows} cells · {}px {} · {} · {}s @ {}fps = {} frames",
        r.width,
        r.height,
        a.font_size,
        names.join(" + "),
        r.theme().name,
        // Show the retiming when --idle-cap/--speed actually changed something,
        // so a shortened video never looks like a shortened recording.
        if (span - c.duration()).abs() > 0.05 {
            format!("{:.1} retimed from {:.1}", span + a.hold, c.duration())
        } else {
            format!("{:.1}", span + a.hold)
        },
        a.fps,
        total,
    );

    let mut t = Term::new(cols, rows);
    let mut buf: Vec<u8> = Vec::new();
    let mut next = 0usize; // next unconsumed event
    let mut last_rev = u64::MAX;
    let started = Instant::now();
    let mut reused = 0usize;

    for f in 0..total {
        let now = f as f64 / a.fps as f64;
        while next < times.len() && times[next] <= now {
            t.feed(&c.events[next].data);
            next += 1;
        }
        // Repaint only when the grid actually changed; a 90-second recording is
        // mostly deliberate pauses, so this is the difference between rendering
        // ~150 frames and ~2700 of them.
        if t.revision != last_rev {
            r.frame(&t, &mut buf);
            last_rev = t.revision;
        } else {
            reused += 1;
        }
        sink.push(&buf, r.width, r.height)?;

        if f % (a.fps as usize * 5) == 0 || f + 1 == total {
            eprint!(
                "\rcast2video: frame {}/{total} ({:.0}%)",
                f + 1,
                (f + 1) as f64 / total as f64 * 100.0
            );
        }
    }
    eprintln!();
    sink.finish()?;

    let painted = total - reused;
    eprintln!(
        "cast2video: wrote {} in {:.1}s ({painted} painted, {reused} reused)",
        a.out.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
