//! The palette, taken from `website/landing/DESIGN-system.md` (adopted
//! 2026-07-31) rather than invented here, so a frame of this video and the HTML
//! player are the same material.
//!
//! The system's governing rule is that **madder is never decorative** — it marks
//! only where policy cut something. So the capture's colours are remapped onto
//! semantic inks instead of being shown as generic terminal ANSI: red is the one
//! code that becomes madder, because the driver only ever uses red for a denial.

/// A semantic ink, not a colour. Resolved against a [`Theme`] at paint time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ink {
    #[default]
    Body,
    /// madder — a thread policy cut.
    Cut,
    /// indigo — a sound thread.
    Sound,
    /// indigo — the narrator's own voice.
    Lead,
    /// indigo — act rules and structure.
    Struct,
    /// indigo-soft — node and type names.
    Thread,
    /// indigo-soft — HTTP request lines.
    Wire,
    /// ink-2 — annotations.
    Note,
}

/// Map an SGR colour parameter onto a semantic ink. Codes the driver never
/// emits are left unmapped so an unexpected one shows as body text rather than
/// silently borrowing a meaning it did not intend.
pub fn ink_for_sgr(code: u16) -> Option<Ink> {
    Some(match code {
        31 => Ink::Cut,
        32 => Ink::Sound,
        33 => Ink::Thread,
        34 => Ink::Wire,
        35 => Ink::Struct,
        36 => Ink::Lead,
        90 => Ink::Note,
        _ => return None,
    })
}

pub type Rgb = [u8; 3];

pub struct Theme {
    pub name: &'static str,
    /// `--sunk`: the terminal well.
    pub bg: Rgb,
    /// Body text in the well.
    pub body: Rgb,
    /// `--ink`: what bold promotes to.
    pub strong: Rgb,
    pub cut: Rgb,
    pub indigo: Rgb,
    pub indigo_soft: Rgb,
    pub note: Rgb,
}

impl Theme {
    pub const DARK: Theme = Theme {
        name: "dark",
        bg: [0x08, 0x0B, 0x0E],
        body: [0xC8, 0xD2, 0xDB],
        strong: [0xE7, 0xEB, 0xEF],
        cut: [0xE5, 0x73, 0x5C],
        indigo: [0x8A, 0xA2, 0xE8],
        indigo_soft: [0x7C, 0x8F, 0xCB],
        note: [0x8B, 0x98, 0xA3],
    };

    pub const LIGHT: Theme = Theme {
        name: "light",
        bg: [0xD2, 0xD9, 0xDE],
        body: [0x1B, 0x24, 0x2C],
        strong: [0x0E, 0x14, 0x1A],
        cut: [0xA6, 0x3A, 0x26],
        indigo: [0x27, 0x38, 0x6B],
        indigo_soft: [0x8F, 0xA0, 0xCE],
        note: [0x53, 0x61, 0x6D],
    };

    pub fn by_name(n: &str) -> Option<&'static Theme> {
        match n {
            "dark" => Some(&Theme::DARK),
            "light" => Some(&Theme::LIGHT),
            _ => None,
        }
    }

    /// Resolve an ink under this theme. `bold` promotes body text to `--ink`;
    /// a coloured ink keeps its hue, since bold is emphasis, not recolouring.
    pub fn resolve(&self, ink: Ink, bold: bool, dim: bool) -> Rgb {
        let c = match ink {
            Ink::Body => {
                if bold {
                    self.strong
                } else {
                    self.body
                }
            }
            Ink::Cut => self.cut,
            Ink::Sound | Ink::Lead | Ink::Struct => self.indigo,
            Ink::Thread | Ink::Wire => self.indigo_soft,
            Ink::Note => self.note,
        };
        if dim { mix(c, self.bg, 0.42) } else { c }
    }
}

/// Blend `a` toward `b` by `t`.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let f = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}
