//! draws a scene into a pixmap with tiny-skia, the selection highlighted

use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::{
    design::{Kind, Scene, Selection, Shape, shown},
    geom::{Point, Rect, Viewport},
};

pub type Rgb = (u8, u8, u8);

/// Eagle's 16 colour palette, on a dark board and on light paper
const BOARD_PALETTE: [Rgb; 16] = [
    (0, 0, 0),
    (50, 80, 210),
    (40, 170, 60),
    (40, 170, 170),
    (200, 50, 50),
    (190, 60, 190),
    (190, 170, 50),
    (185, 185, 185),
    (95, 95, 95),
    (90, 140, 255),
    (90, 240, 90),
    (90, 235, 235),
    (255, 90, 90),
    (255, 100, 255),
    (250, 240, 90),
    (240, 240, 240),
];

const SHEET_PALETTE: [Rgb; 16] = [
    (0, 0, 0),
    (40, 40, 170),
    (20, 130, 40),
    (0, 125, 125),
    (165, 25, 25),
    (140, 20, 140),
    (130, 105, 0),
    (105, 105, 105),
    (60, 60, 60),
    (40, 70, 230),
    (10, 150, 30),
    (0, 150, 150),
    (210, 40, 40),
    (190, 30, 190),
    (175, 140, 0),
    (25, 25, 25),
];

pub const BOARD_BACKGROUND: Rgb = (14, 16, 20);
pub const SHEET_BACKGROUND: Rgb = (250, 249, 244);
pub const HIGHLIGHT: Rgb = (255, 140, 0);

pub fn background(kind: Kind) -> Rgb {
    match kind {
        Kind::Schematic => SHEET_BACKGROUND,
        Kind::Board => BOARD_BACKGROUND,
    }
}

fn color(rgb: Rgb, alpha: f32) -> Color {
    Color::from_rgba8(rgb.0, rgb.1, rgb.2, (alpha * 255.0) as u8)
}

fn layer_rgb(scene: &Scene, layer: u16) -> Rgb {
    let palette = match scene.kind {
        Kind::Schematic => &SHEET_PALETTE,
        Kind::Board => &BOARD_PALETTE,
    };
    let index = scene.layer_colors.get(&layer).copied().unwrap_or(7) as usize;
    let rgb = palette[index % 16];
    // black on black would vanish
    if rgb == (0, 0, 0) && scene.kind == Kind::Board { palette[7] } else { rgb }
}

/// draws `scene` as seen through `view` into a new pixmap of the view's size
pub fn render(scene: &Scene, view: &Viewport, selection: Selection) -> Pixmap {
    let mut pixmap = Pixmap::new(view.width.max(1), view.height.max(1)).expect("pixmap size");
    let bg = background(scene.kind);
    pixmap.fill(color(bg, 1.0));
    let mut painter = Painter {
        pixmap: &mut pixmap,
        view: *view,
        visible: view.visible().inflate(1.0),
    };
    for item in &scene.items {
        if !shown(scene.kind, item.layer) {
            continue;
        }
        let rgb = layer_rgb(scene, item.layer);
        let alpha = match (scene.kind, item.layer) {
            // copper is see-through so the other side shows
            (Kind::Board, 1..=16) => 0.8,
            _ => 1.0,
        };
        painter.draw(&item.shape, item.bounds, rgb, alpha, bg);
    }
    if selection != Selection::None {
        // dim the rest, then draw the selection on top
        painter.pixmap.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, view.width as f32, view.height as f32).unwrap(),
            &paint(color(bg, 0.6)),
            Transform::identity(),
            None,
        );
        for item in scene.items.iter().filter(|i| selection.matches(i) && shown(scene.kind, i.layer)) {
            painter.draw(&item.shape, item.bounds, HIGHLIGHT, 1.0, bg);
        }
        if let Selection::Part(part) = selection
            && let Some(rect) = scene.part_bounds.get(&part)
        {
            painter.frame(*rect, HIGHLIGHT);
        }
    }
    pixmap
}

fn paint(color: Color) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    paint
}

struct Painter<'a> {
    pixmap: &'a mut Pixmap,
    view: Viewport,
    visible: Rect,
}

impl Painter<'_> {
    fn px(&self, p: Point) -> (f32, f32) {
        let s = self.view.to_screen(p);
        (s.x as f32, s.y as f32)
    }

    /// a world width in pixels, at least a hairline
    fn width(&self, w: f64) -> f32 {
        ((w * self.view.scale) as f32).max(1.0)
    }

    fn stroke(&mut self, pts: &[Point], width: f32, flat: bool, closed: bool, color: Color) {
        let mut pb = PathBuilder::new();
        for (i, p) in pts.iter().enumerate() {
            let (x, y) = self.px(*p);
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        if pts.len() == 1 {
            let (x, y) = self.px(pts[0]);
            pb.line_to(x + 0.01, y);
        }
        if closed {
            pb.close();
        }
        let Some(path) = pb.finish() else { return };
        let stroke = Stroke {
            width,
            line_cap: if flat { LineCap::Butt } else { LineCap::Round },
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        self.pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }

    fn fill(&mut self, pts: &[Point], color: Color) {
        let mut pb = PathBuilder::new();
        for (i, p) in pts.iter().enumerate() {
            let (x, y) = self.px(*p);
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
        }
    }

    fn circle(&mut self, c: Point, r: f64, width: f64, color: Color) {
        let (x, y) = self.px(c);
        let r = (r * self.view.scale) as f32;
        if width == 0.0 {
            if let Some(path) = PathBuilder::from_circle(x, y, r.max(0.5)) {
                self.pixmap.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
            }
        } else if let Some(path) = PathBuilder::from_circle(x, y, r.max(0.5)) {
            let stroke = Stroke {
                width: self.width(width),
                ..Stroke::default()
            };
            self.pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
        }
    }

    fn draw(&mut self, shape: &Shape, bounds: Rect, rgb: Rgb, alpha: f32, bg: Rgb) {
        if !bounds.intersects(&self.visible) {
            return;
        }
        let c = color(rgb, alpha);
        match shape {
            Shape::Line { pts, width, flat } => self.stroke(pts, self.width(*width), *flat, false, c),
            Shape::Circle { c: center, r, width } => self.circle(*center, *r, *width, c),
            Shape::Poly { pts, width, pour } => {
                if *pour {
                    // its poured copper is not in the file: a light tint of its area
                    self.fill(pts, color(rgb, alpha * 0.14));
                    self.stroke(pts, self.width(*width).min(2.0), false, true, c);
                } else {
                    self.fill(pts, c);
                    if *width > 0.0 {
                        self.stroke(pts, self.width(*width), false, true, c);
                    }
                }
            }
            Shape::Text(text) => {
                // too small to read: skip
                if text.size * self.view.scale < 2.5 {
                    return;
                }
                text.draw(self.pixmap, self.view.xform(), &paint(c));
            }
            Shape::Drill { c: center, r } => {
                self.circle(*center, *r, 0.0, color(bg, 1.0));
            }
        }
    }

    /// a dashed frame around a selected part
    fn frame(&mut self, rect: Rect, rgb: Rgb) {
        let pad = 3.0 / self.view.scale;
        let r = rect.inflate(pad);
        let (x0, y0) = self.px(Point::new(r.min.x, r.max.y));
        let (x1, y1) = self.px(Point::new(r.max.x, r.min.y));
        let Some(rect) = tiny_skia::Rect::from_ltrb(x0, y0, x1, y1) else { return };
        let path = PathBuilder::from_rect(rect);
        let stroke = Stroke {
            width: 1.5,
            dash: tiny_skia::StrokeDash::new(vec![6.0, 4.0], 0.0),
            ..Stroke::default()
        };
        self.pixmap.stroke_path(&path, &paint(color(rgb, 1.0)), &stroke, Transform::identity(), None);
    }
}
