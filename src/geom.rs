//! world geometry (millimetres, y up, like Eagle) and the world <-> screen viewport

use std::ops::{Add, Mul, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn dist(self, other: Point) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }

    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f64> for Point {
    type Output = Point;
    fn mul(self, k: f64) -> Point {
        Point::new(self.x * k, self.y * k)
    }
}

/// distance from `p` to the segment `a`-`b`
pub fn segment_dist(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let len2 = ab.x * ab.x + ab.y * ab.y;
    if len2 == 0.0 {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len2).clamp(0.0, 1.0);
    p.dist(a + ab * t)
}

/// points of the arc Eagle draws from `a` to `b` turning `curve` degrees
/// (counter-clockwise when positive)
pub fn arc_points(a: Point, b: Point, curve: f64) -> Vec<Point> {
    let chord = b - a;
    let len = chord.len();
    if curve == 0.0 || len == 0.0 {
        return vec![a, b];
    }
    let theta = curve.to_radians();
    let normal = Point::new(-chord.y / len, chord.x / len);
    let center = (a + b) * 0.5 + normal * (len * 0.5 / (theta * 0.5).tan());
    let radius = a.dist(center);
    let start = (a.y - center.y).atan2(a.x - center.x);
    let steps = ((curve.abs() / 6.0).ceil() as usize).clamp(4, 90);
    (0..=steps)
        .map(|i| {
            let t = start + theta * i as f64 / steps as f64;
            Point::new(center.x + radius * t.cos(), center.y + radius * t.sin())
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

impl Default for Rect {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Rect {
    pub const EMPTY: Rect = Rect {
        min: Point::new(f64::INFINITY, f64::INFINITY),
        max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
    };

    pub fn around(c: Point, r: f64) -> Self {
        Rect {
            min: Point::new(c.x - r, c.y - r),
            max: Point::new(c.x + r, c.y + r),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    pub fn include(&mut self, p: Point) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    pub fn union(&mut self, other: Rect) {
        if !other.is_empty() {
            self.include(other.min);
            self.include(other.max);
        }
    }

    pub fn inflate(self, d: f64) -> Rect {
        if self.is_empty() {
            return self;
        }
        Rect {
            min: Point::new(self.min.x - d, self.min.y - d),
            max: Point::new(self.max.x + d, self.max.y + d),
        }
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.min.x <= o.max.x && o.min.x <= self.max.x && self.min.y <= o.max.y && o.min.y <= self.max.y
    }

    pub fn center(&self) -> Point {
        (self.min + self.max) * 0.5
    }

    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }

    pub fn area(&self) -> f64 {
        self.width().max(0.0) * self.height().max(0.0)
    }
}

/// an Eagle orientation like `R90`, `MR180` (mirrored) or `SR45` (spin: text stays as is)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rot {
    pub angle: f64,
    pub mirror: bool,
    pub spin: bool,
}

impl Rot {
    pub fn parse(s: &str) -> Rot {
        let mut rot = Rot::default();
        let mut chars = s.trim();
        while let Some(c) = chars.chars().next() {
            match c {
                'M' => rot.mirror = true,
                'S' => rot.spin = true,
                'R' => {
                    rot.angle = chars[1..].parse().unwrap_or(0.0);
                    break;
                }
                _ => break,
            }
            chars = &chars[1..];
        }
        rot
    }

    pub fn to_eagle(self) -> String {
        format!(
            "{}{}R{}",
            if self.spin { "S" } else { "" },
            if self.mirror { "M" } else { "" },
            self.angle
        )
    }
}

/// affine map: x' = a x + c y + e, y' = b x + d y + f
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Xform {
    pub const IDENTITY: Xform = Xform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Eagle placement: mirror about the y axis, then rotate, then move to `at`
    pub fn place(at: Point, rot: Rot) -> Xform {
        let (s, c) = rot.angle.to_radians().sin_cos();
        let m = if rot.mirror { -1.0 } else { 1.0 };
        Xform {
            a: c * m,
            b: s * m,
            c: -s,
            d: c,
            e: at.x,
            f: at.y,
        }
    }

    pub fn translate(x: f64, y: f64) -> Xform {
        Xform {
            e: x,
            f: y,
            ..Xform::IDENTITY
        }
    }

    pub fn scale(k: f64) -> Xform {
        Xform {
            a: k,
            d: k,
            ..Xform::IDENTITY
        }
    }

    /// `self ∘ inner`: applies `inner` first
    pub fn then(self, inner: Xform) -> Xform {
        let s = self;
        Xform {
            a: s.a * inner.a + s.c * inner.b,
            b: s.b * inner.a + s.d * inner.b,
            c: s.a * inner.c + s.c * inner.d,
            d: s.b * inner.c + s.d * inner.d,
            e: s.a * inner.e + s.c * inner.f + s.e,
            f: s.b * inner.e + s.d * inner.f + s.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    pub fn mirrored(&self) -> bool {
        self.a * self.d - self.b * self.c < 0.0
    }

    /// rotation in degrees of the placement (the mirror applies before it)
    pub fn angle(&self) -> f64 {
        let angle = (-self.c).atan2(self.d).to_degrees();
        let angle = angle.rem_euclid(360.0);
        if (angle - 360.0).abs() < 1e-6 { 0.0 } else { angle }
    }

    pub fn to_skia(self) -> tiny_skia::Transform {
        tiny_skia::Transform::from_row(
            self.a as f32,
            self.b as f32,
            self.c as f32,
            self.d as f32,
            self.e as f32,
            self.f as f32,
        )
    }
}

/// maps world millimetres to the pixels of a canvas
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// world point at the middle of the canvas
    pub center: Point,
    /// pixels per millimetre
    pub scale: f64,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn fit(rect: Rect, width: u32, height: u32) -> Viewport {
        let mut view = Viewport {
            center: Point::default(),
            scale: 1.0,
            width,
            height,
        };
        view.fit_rect(rect, 0.04);
        view
    }

    /// shows `rect` with a margin of `margin` times the canvas size
    pub fn fit_rect(&mut self, rect: Rect, margin: f64) {
        if rect.is_empty() {
            return;
        }
        let w = self.width as f64 * (1.0 - 2.0 * margin);
        let h = self.height as f64 * (1.0 - 2.0 * margin);
        self.scale = (w / rect.width().max(1e-3))
            .min(h / rect.height().max(1e-3))
            .clamp(0.05, 5000.0);
        self.center = rect.center();
    }

    pub fn to_screen(self, p: Point) -> Point {
        Point::new(
            (p.x - self.center.x) * self.scale + self.width as f64 * 0.5,
            self.height as f64 * 0.5 - (p.y - self.center.y) * self.scale,
        )
    }

    pub fn to_world(self, x: f64, y: f64) -> Point {
        Point::new(
            (x - self.width as f64 * 0.5) / self.scale + self.center.x,
            (self.height as f64 * 0.5 - y) / self.scale + self.center.y,
        )
    }

    /// the world to screen transform
    pub fn xform(&self) -> Xform {
        Xform {
            a: self.scale,
            b: 0.0,
            c: 0.0,
            d: -self.scale,
            e: self.width as f64 * 0.5 - self.center.x * self.scale,
            f: self.height as f64 * 0.5 + self.center.y * self.scale,
        }
    }

    /// zooms by `factor` keeping the world point under the pixel (x, y) in place
    pub fn zoom_at(&mut self, factor: f64, x: f64, y: f64) {
        let anchor = self.to_world(x, y);
        self.scale = (self.scale * factor).clamp(0.05, 5000.0);
        self.center = Point::new(
            anchor.x - (x - self.width as f64 * 0.5) / self.scale,
            anchor.y + (y - self.height as f64 * 0.5) / self.scale,
        );
    }

    /// moves the content by (dx, dy) pixels
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.center.x -= dx / self.scale;
        self.center.y += dy / self.scale;
    }

    pub fn visible(&self) -> Rect {
        let mut rect = Rect::EMPTY;
        rect.include(self.to_world(0.0, 0.0));
        rect.include(self.to_world(self.width as f64, self.height as f64));
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rot_parse() {
        assert_eq!(
            Rot::parse("MR90"),
            Rot {
                angle: 90.0,
                mirror: true,
                spin: false
            }
        );
        assert_eq!(
            Rot::parse("SR180"),
            Rot {
                angle: 180.0,
                mirror: false,
                spin: true
            }
        );
        assert_eq!(Rot::parse("R270").angle, 270.0);
        assert_eq!(Rot::parse(""), Rot::default());
    }

    #[test]
    fn place_and_angle() {
        let x = Xform::place(Point::new(10.0, 0.0), Rot::parse("R90"));
        let p = x.apply(Point::new(1.0, 0.0));
        assert!((p.x - 10.0).abs() < 1e-9 && (p.y - 1.0).abs() < 1e-9);
        assert!((x.angle() - 90.0).abs() < 1e-9);
        let m = Xform::place(Point::default(), Rot::parse("MR90"));
        assert!(m.mirrored());
        assert!((m.angle() - 90.0).abs() < 1e-9);
        let both =
            Xform::place(Point::default(), Rot::parse("MR0")).then(Xform::place(Point::default(), Rot::parse("R90")));
        assert!(both.mirrored());
        assert!((both.angle() - 270.0).abs() < 1e-9);
    }

    #[test]
    fn arc_quarter() {
        let pts = arc_points(Point::new(1.0, 0.0), Point::new(0.0, 1.0), 90.0);
        for p in &pts {
            assert!((p.len() - 1.0).abs() < 1e-9);
        }
        assert!(pts.last().unwrap().dist(Point::new(0.0, 1.0)) < 1e-9);
    }

    #[test]
    fn viewport_roundtrip() {
        let mut v = Viewport::fit(
            Rect {
                min: Point::new(0.0, 0.0),
                max: Point::new(100.0, 50.0),
            },
            800,
            400,
        );
        let s = v.to_screen(Point::new(20.0, 30.0));
        let w = v.to_world(s.x, s.y);
        assert!(w.dist(Point::new(20.0, 30.0)) < 1e-9);
        v.zoom_at(2.0, 100.0, 100.0);
        let before = v.to_world(100.0, 100.0);
        v.zoom_at(1.5, 100.0, 100.0);
        assert!(v.to_world(100.0, 100.0).dist(before) < 1e-9);
    }
}
