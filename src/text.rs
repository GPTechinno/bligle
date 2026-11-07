//! text drawn from the outlines of a system font, so it scales, rotates and mirrors
//! like Eagle's texts

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap};

use crate::{
    eagle::Align,
    geom::{Point, Rot, Xform},
};

pub struct Font {
    pub data: Arc<[u8]>,
    index: u32,
    /// capital height in font units: Eagle's text size is the height of capitals
    cap: f64,
    glyphs: Mutex<HashMap<char, Glyph>>,
}

#[derive(Clone)]
struct Glyph {
    advance: f64,
    path: Option<Arc<Path>>,
}

static FONT: OnceLock<Option<Font>> = OnceLock::new();

/// the font used to draw texts (looked up once among the system fonts)
pub fn font() -> Option<&'static Font> {
    FONT.get_or_init(load).as_ref()
}

fn load() -> Option<Font> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let families = [
        fontdb::Family::Name("DejaVu Sans"),
        fontdb::Family::Name("Liberation Sans"),
        fontdb::Family::Name("Noto Sans"),
        fontdb::Family::Name("Helvetica"),
        fontdb::Family::Name("Arial"),
        fontdb::Family::SansSerif,
    ];
    let id = db.query(&fontdb::Query {
        families: &families,
        ..fontdb::Query::default()
    })?;
    let (data, index) = db.with_face_data(id, |data, index| (data.to_vec(), index))?;
    let face = ttf_parser::Face::parse(&data, index).ok()?;
    let cap = face
        .capital_height()
        .filter(|&c| c > 0)
        .map(f64::from)
        .unwrap_or(face.units_per_em() as f64 * 0.7);
    Some(Font {
        data: Arc::from(data),
        index,
        cap,
        glyphs: Mutex::new(HashMap::new()),
    })
}

struct Outline(PathBuilder);

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

impl Font {
    fn glyph(&self, c: char) -> Glyph {
        if let Some(glyph) = self.glyphs.lock().unwrap().get(&c) {
            return glyph.clone();
        }
        let glyph = ttf_parser::Face::parse(&self.data, self.index)
            .ok()
            .and_then(|face| {
                let id = face.glyph_index(c).or_else(|| face.glyph_index('?'))?;
                let advance = face.glyph_hor_advance(id).unwrap_or(0) as f64;
                let mut outline = Outline(PathBuilder::new());
                face.outline_glyph(id, &mut outline);
                Some(Glyph {
                    advance,
                    path: outline.0.finish().map(Arc::new),
                })
            })
            .unwrap_or(Glyph {
                advance: self.cap * 0.6,
                path: None,
            });
        self.glyphs.lock().unwrap().insert(c, glyph.clone());
        glyph
    }

    /// width of one line of text of the given size
    pub fn width(&self, line: &str, size: f64) -> f64 {
        line.chars().map(|c| self.glyph(c).advance).sum::<f64>() * size / self.cap
    }
}

/// width of one line of text, without a font: an estimate
pub fn line_width(line: &str, size: f64) -> f64 {
    match font() {
        Some(font) => font.width(line, size),
        None => line.chars().count() as f64 * size * 0.75,
    }
}

/// a text placed in the world, ready to draw
#[derive(Clone, Debug)]
pub struct Placed {
    pub at: Point,
    pub text: String,
    pub size: f64,
    /// final rotation (already turned to read upright) and mirror
    pub rot: Rot,
    pub align: Align,
    /// line pitch in percent of the size
    pub distance: f64,
}

impl Placed {
    /// a text with an Eagle orientation as composed through its placement: Eagle turns
    /// texts by 180° when they would read upside down (unless they spin)
    pub fn new(at: Point, text: String, size: f64, xform: Xform, spin: bool, mut align: Align, distance: f64) -> Placed {
        let mut angle = xform.angle();
        if !spin && angle > 90.0 + 1e-6 && angle <= 270.0 + 1e-6 {
            angle -= 180.0;
            align = Align {
                h: -align.h,
                v: -align.v,
            };
        }
        Placed {
            at,
            text,
            size,
            rot: Rot {
                angle,
                mirror: xform.mirrored(),
                spin,
            },
            align,
            distance,
        }
    }

    fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.split('\n')
    }

    fn pitch(&self) -> f64 {
        self.size * (1.0 + self.distance / 100.0)
    }

    /// the text's corners in the world (for culling and picking)
    pub fn corners(&self) -> [Point; 4] {
        let count = self.lines().count().max(1);
        let width = self.lines().map(|l| line_width(l, self.size)).fold(0.0, f64::max);
        let height = self.size + (count - 1) as f64 * self.pitch();
        let x0 = match self.align.h {
            -1 => 0.0,
            0 => -width * 0.5,
            _ => -width,
        };
        let y0 = match self.align.v {
            -1 => 0.0,
            0 => -height * 0.5,
            _ => -height,
        };
        let place = Xform::place(self.at, self.rot);
        [
            place.apply(Point::new(x0, y0)),
            place.apply(Point::new(x0 + width, y0)),
            place.apply(Point::new(x0 + width, y0 + height)),
            place.apply(Point::new(x0, y0 + height)),
        ]
    }

    /// draws the text with the world to screen transform `screen`
    pub fn draw(&self, pixmap: &mut Pixmap, screen: Xform, paint: &Paint) {
        let Some(font) = font() else { return };
        let count = self.lines().count();
        let pitch = self.pitch();
        let height = self.size + (count.max(1) - 1) as f64 * pitch;
        let bottom = match self.align.v {
            -1 => 0.0,
            0 => -height * 0.5,
            _ => -height,
        };
        let k = self.size / font.cap;
        let place = screen.then(Xform::place(self.at, self.rot));
        for (i, line) in self.lines().enumerate() {
            let width = font.width(line, self.size);
            let mut x = match self.align.h {
                -1 => 0.0,
                0 => -width * 0.5,
                _ => -width,
            };
            let baseline = bottom + (count - 1 - i) as f64 * pitch;
            for c in line.chars() {
                let glyph = font.glyph(c);
                if let Some(path) = &glyph.path {
                    let transform = place.then(Xform::translate(x, baseline)).then(Xform::scale(k));
                    pixmap.fill_path(path, paint, FillRule::Winding, transform.to_skia(), None);
                }
                x += glyph.advance * k;
            }
        }
    }
}

/// Eagle text markup: `!` toggles an overbar, `\!` is a literal `!`
pub fn strip_overbar(text: &str) -> String {
    if !text.contains('!') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'!') => {
                out.push('!');
                chars.next();
            }
            '!' => {}
            _ => out.push(c),
        }
    }
    out
}
