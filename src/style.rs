//! SGR styles as data: build escape sequences from a [`Style`], and apply
//! parsed SGR parameters back onto one — the two directions used by the
//! screen renderer and by anything interpreting terminal output.

/// A terminal color: the terminal's default, one of the 256 indexed colors
/// (0–7 normal, 8–15 bright, 16–255 the extended cube/grays), or 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// A complete character style: foreground, background, and attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strike: bool,
}

impl Style {
    /// The absolute SGR sequence for this style: a reset followed by every
    /// set attribute, so it renders identically regardless of prior state.
    /// The default style renders as `ESC[0m`.
    pub fn sgr(&self) -> String {
        let mut params = String::from("0");
        self.push_set_params(&mut params);
        format!("\x1b[{params}m")
    }

    /// The minimal SGR sequence that changes a terminal currently displaying
    /// `self` to display `to`. Returns `""` when the styles are equal. When
    /// `to` only *adds* attributes, only the additions are emitted; when
    /// anything must be turned off, the absolute form of `to` is emitted
    /// (reset + set), which is always correct.
    pub fn transition_to(&self, to: &Style) -> String {
        if self == to {
            return String::new();
        }
        let removes = (self.bold && !to.bold)
            || (self.dim && !to.dim)
            || (self.italic && !to.italic)
            || (self.underline && !to.underline)
            || (self.reverse && !to.reverse)
            || (self.strike && !to.strike)
            || (self.fg != to.fg && to.fg == Color::Default)
            || (self.bg != to.bg && to.bg == Color::Default);
        if removes {
            return to.sgr();
        }
        let mut params = String::new();
        let diff = Style {
            fg: if self.fg != to.fg {
                to.fg
            } else {
                Color::Default
            },
            bg: if self.bg != to.bg {
                to.bg
            } else {
                Color::Default
            },
            bold: to.bold && !self.bold,
            dim: to.dim && !self.dim,
            italic: to.italic && !self.italic,
            underline: to.underline && !self.underline,
            reverse: to.reverse && !self.reverse,
            strike: to.strike && !self.strike,
        };
        diff.push_set_params(&mut params);
        debug_assert!(!params.is_empty());
        format!("\x1b[{}m", params.trim_start_matches(';'))
    }

    /// Append the SGR parameters for every *set* attribute (`;`-prefixed).
    fn push_set_params(&self, params: &mut String) {
        use std::fmt::Write;
        if self.bold {
            params.push_str(";1");
        }
        if self.dim {
            params.push_str(";2");
        }
        if self.italic {
            params.push_str(";3");
        }
        if self.underline {
            params.push_str(";4");
        }
        if self.reverse {
            params.push_str(";7");
        }
        if self.strike {
            params.push_str(";9");
        }
        match self.fg {
            Color::Default => {}
            Color::Indexed(n @ 0..=7) => write!(params, ";{}", 30 + n as u16).unwrap(),
            Color::Indexed(n @ 8..=15) => write!(params, ";{}", 90 + (n - 8) as u16).unwrap(),
            Color::Indexed(n) => write!(params, ";38;5;{n}").unwrap(),
            Color::Rgb(r, g, b) => write!(params, ";38;2;{r};{g};{b}").unwrap(),
        }
        match self.bg {
            Color::Default => {}
            Color::Indexed(n @ 0..=7) => write!(params, ";{}", 40 + n as u16).unwrap(),
            Color::Indexed(n @ 8..=15) => write!(params, ";{}", 100 + (n - 8) as u16).unwrap(),
            Color::Indexed(n) => write!(params, ";48;5;{n}").unwrap(),
            Color::Rgb(r, g, b) => write!(params, ";48;2;{r};{g};{b}").unwrap(),
        }
    }

    /// Apply parsed SGR parameters (the numbers from `ESC[...m`) to this
    /// style, as a terminal would. An empty parameter list means reset.
    /// Unknown parameters are ignored; malformed 38/48 extended-color runs
    /// consume what they can and stop.
    pub fn apply_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            *self = Style::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => *self = Style::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                7 => self.reverse = true,
                9 => self.strike = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                27 => self.reverse = false,
                29 => self.strike = false,
                30..=37 => self.fg = Color::Indexed((params[i] - 30) as u8),
                39 => self.fg = Color::Default,
                90..=97 => self.fg = Color::Indexed((params[i] - 90 + 8) as u8),
                40..=47 => self.bg = Color::Indexed((params[i] - 40) as u8),
                49 => self.bg = Color::Default,
                100..=107 => self.bg = Color::Indexed((params[i] - 100 + 8) as u8),
                38 | 48 => {
                    let target_fg = params[i] == 38;
                    let color = match params.get(i + 1) {
                        Some(5) => {
                            let Some(&n) = params.get(i + 2) else { return };
                            i += 2;
                            Color::Indexed(n.min(255) as u8)
                        }
                        Some(2) => {
                            let (Some(&r), Some(&g), Some(&b)) =
                                (params.get(i + 2), params.get(i + 3), params.get(i + 4))
                            else {
                                return;
                            };
                            i += 4;
                            Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8)
                        }
                        _ => return,
                    };
                    if target_fg {
                        self.fg = color;
                    } else {
                        self.bg = color;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}
