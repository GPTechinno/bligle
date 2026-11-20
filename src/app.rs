//! the viewer: the schematic and the board side by side, one selection shared by both

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use blit::{Atom, AtomCx, Constraints, Id, Input, Key, KeyInput, LogicalRect, MouseButton, Sides, Size};
use blit_desktop::Ui;
use blit_gui::{
    BoundsClip, Context,
    atom::Rectangle,
    color::Color,
    image::{
        ImageData, ImageFit, ImageFormat, ImageHandle, ImageId, ImagePixels, ImageRequest, ImageSampling, ImageTiling,
    },
    style::BorderRadius,
    text::{TextOptions, TextOverflow, TextStyle},
};
use blit_gui_widgets::{
    Text, interact,
    layout::{Align, Justify, Sizing, flex},
};

use crate::{
    design::{Design, Kind, Scene, Selection},
    geom::{Rect, Viewport},
    render::{self, HIGHLIGHT, Rgb},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Sch,
    Brd,
}

mod colors {
    use blit_gui::color::Color;
    pub const BACKGROUND: Color = Color::rgb(22, 24, 29);
    pub const PANEL: Color = Color::rgb(32, 35, 42);
    pub const CHIP: Color = Color::rgb(46, 50, 60);
    pub const CHIP_HOVER: Color = Color::rgb(62, 67, 80);
    pub const TEXT: Color = Color::rgb(222, 225, 230);
    pub const MUTED: Color = Color::rgb(140, 146, 158);
    pub const DIVIDER: Color = Color::rgb(10, 11, 14);
}

fn rgb((r, g, b): Rgb) -> Color {
    Color::rgb(r, g, b)
}

fn style(size: f32, color: Color) -> TextStyle<'static> {
    TextStyle::new().size(size).color(color)
}

/// the image last rendered for a canvas and what it shows
struct Shown {
    image: ImageHandle,
    view: Viewport,
    selection: Selection,
    sheet: usize,
}

#[derive(Default)]
struct Canvas {
    view: Option<Viewport>,
    shown: Option<Shown>,
}

pub struct App {
    pub design: Arc<Design>,
    pub sheet: usize,
    pub selection: Selection,
    sch: Canvas,
    brd: Canvas,
    hover: (Side, Selection),
    search: Option<String>,
    message: Option<String>,
    last_click: Option<(Instant, f32, f32)>,
    /// right / middle button drag in progress (blit's interactions track the primary one)
    raw_drag: Option<(Side, f32, f32)>,
    /// the UI scale the renderer uses, so canvases render at the physical resolution
    scale: Arc<AtomicU32>,
    /// reveal the selection on these sides once their canvas has a size (zoom when true)
    pending_reveal: Vec<(Side, bool)>,
}

/// the scene a side shows
fn scene(design: &Design, sheet: usize, side: Side) -> Option<&Scene> {
    match side {
        Side::Sch => design.sheets.get(sheet),
        Side::Brd => design.board.as_ref(),
    }
}

impl App {
    pub fn new(design: Arc<Design>, scale: Arc<AtomicU32>) -> App {
        App {
            design,
            sheet: 0,
            selection: Selection::None,
            sch: Canvas::default(),
            brd: Canvas::default(),
            hover: (Side::Sch, Selection::None),
            search: None,
            message: None,
            last_click: None,
            raw_drag: None,
            scale,
            pending_reveal: Vec::new(),
        }
    }

    fn ui_scale(&self) -> f32 {
        f32::from_bits(self.scale.load(Ordering::Relaxed))
    }

    fn canvas_mut(&mut self, side: Side) -> &mut Canvas {
        match side {
            Side::Sch => &mut self.sch,
            Side::Brd => &mut self.brd,
        }
    }

    /// selects a part or net by name (from the search or the command line)
    pub fn select_named(&mut self, name: &str) -> bool {
        let selection = self.design.find(name);
        if selection == Selection::None {
            self.message = Some(format!("nothing named {name:?}"));
            return false;
        }
        self.message = None;
        self.select(selection, None, true);
        true
    }

    pub fn set_sheet(&mut self, sheet: usize) {
        if sheet < self.design.sheets.len() && sheet != self.sheet {
            self.sheet = sheet;
        }
    }

    /// selects `selection` from `from` (None: from the search) and brings it into view
    /// on the other side, or both when zooming
    fn select(&mut self, selection: Selection, from: Option<Side>, zoom: bool) {
        self.selection = selection;
        if selection == Selection::None {
            return;
        }
        // the schematic follows to the sheet showing the selection
        if from != Some(Side::Sch) {
            let sheets: Vec<usize> = match selection {
                Selection::Part(p) => self.design.sch_locations(p).into_iter().map(|(s, _)| s).collect(),
                Selection::Net(n) => self.design.net_sheets(n),
                Selection::None => Vec::new(),
            };
            if !sheets.is_empty() && !sheets.contains(&self.sheet) {
                self.sheet = sheets[0];
            }
        }
        for side in [Side::Sch, Side::Brd] {
            if from != Some(side) || zoom {
                self.pending_reveal.push((side, zoom));
            }
        }
    }

    /// moves a canvas so the selection shows, zooming on it when asked
    fn reveal(&mut self, side: Side, zoom: bool) {
        let Some(scene) = scene(&self.design, self.sheet, side) else {
            return;
        };
        let rect = scene.selection_bounds(self.selection);
        if rect.is_empty() {
            return;
        }
        let min = match scene.kind {
            Kind::Schematic => 50.0,
            Kind::Board => 12.0,
        };
        let canvas = match side {
            Side::Sch => &mut self.sch,
            Side::Brd => &mut self.brd,
        };
        let Some(view) = &mut canvas.view else { return };
        let visible = view.visible();
        let fits = visible.contains(rect.min) && visible.contains(rect.max);
        if zoom || rect.width() > visible.width() || rect.height() > visible.height() {
            let grown = Rect::around(rect.center(), (rect.width().max(rect.height()) * 0.5).max(min * 0.5));
            let mut target = rect;
            target.union(grown);
            view.fit_rect(target, 0.08);
        } else if !fits {
            view.center = rect.center();
        }
    }

    fn fit_all(&mut self) {
        for side in [Side::Sch, Side::Brd] {
            let bounds = scene(&self.design, self.sheet, side).map(|s| s.bounds);
            if let (Some(bounds), Some(view)) = (bounds, &mut self.canvas_mut(side).view) {
                view.fit_rect(bounds, 0.04);
            }
        }
    }

    // ------------------------------------------------------------------ frame

    pub fn render(&mut self, ui: Ui<'_>) {
        let input = ui.input();
        self.keys(input);
        let mut root = ui.layout(flex::column().align(Align::Stretch));
        root.insert(Rectangle::new().background(colors::BACKGROUND));
        root.child()
            .item(flex::item().width(Sizing::grow()))
            .build(|ui: Ui<'_>| self.top_bar(ui));
        root.child().item(flex::item().grow()).build(|ui: Ui<'_>| {
            let mut row = ui.layout(flex::row().align(Align::Stretch));
            row.child()
                .item(flex::item().grow())
                .build(|ui: Ui<'_>| self.pane(ui, Side::Sch));
            row.child()
                .item(flex::item().width(Sizing::fixed(2.0)).height(Sizing::grow()))
                .build(|mut ui: Ui<'_>| ui.insert(Rectangle::new().background(colors::DIVIDER)));
            row.child()
                .item(flex::item().grow())
                .build(|ui: Ui<'_>| self.pane(ui, Side::Brd));
        });
        root.child()
            .item(flex::item().width(Sizing::grow()))
            .build(|ui: Ui<'_>| self.info_bar(ui));
    }

    fn keys(&mut self, input: Input<f32>) {
        if let Some(query) = &mut self.search {
            match input {
                Input::Text(c) if !c.is_control() => query.push(c),
                Input::Key(KeyInput { key, pressed: true, .. }) => match key {
                    Key::Backspace => {
                        query.pop();
                    }
                    Key::Escape => self.search = None,
                    Key::Enter => {
                        let query = self.search.take().unwrap_or_default();
                        if !query.trim().is_empty() {
                            self.select_named(&query);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
            return;
        }
        match input {
            Input::Text('/') => {
                self.search = Some(String::new());
                self.message = None;
            }
            Input::Key(KeyInput {
                key,
                pressed: true,
                modifiers,
                ..
            }) => match key {
                Key::Character('f' | 'F') if modifiers.control() => self.search = Some(String::new()),
                Key::Character('f' | 'F') => self.fit_all(),
                Key::Character('z' | 'Z') => {
                    self.pending_reveal.push((Side::Sch, true));
                    self.pending_reveal.push((Side::Brd, true));
                }
                Key::Escape => self.selection = Selection::None,
                Key::PageDown => self.set_sheet(self.sheet + 1),
                Key::PageUp => self.set_sheet(self.sheet.saturating_sub(1)),
                _ => {}
            },
            _ => {}
        }
    }

    fn top_bar(&mut self, ui: Ui<'_>) {
        let mut bar = ui.layout(flex::row().padding(Sides::xy(12.0, 7.0)).gap(12.0).align(Align::Center));
        bar.insert(Rectangle::new().background(colors::PANEL));
        bar.child()
            .insert(Text::new("bligle").style(style(15.0, rgb(HIGHLIGHT)).weight(700)));
        bar.child()
            .insert(Text::new(&self.design.name).style(style(14.0, colors::TEXT)));
        bar.child()
            .item(flex::item().width(Sizing::grow()))
            .build(|_: Ui<'_>| {});
        let search = match &self.search {
            Some(query) => format!("find: {query}▏"),
            None => "/ to find a part or net".to_string(),
        };
        let open = self.search.is_some();
        let clicked = bar.child().build(|mut ui: Ui<'_>| {
            let press = ui.interact(interact::press());
            let mut field = ui.layout(flex::row().padding(Sides::xy(10.0, 4.0)));
            field.insert(
                Rectangle::new()
                    .background(if open || press.hovered {
                        colors::CHIP_HOVER
                    } else {
                        colors::CHIP
                    })
                    .radius(BorderRadius::uniform(4.0)),
            );
            field
                .child()
                .insert(Text::new(&search).style(style(13.0, if open { colors::TEXT } else { colors::MUTED })));
            press.clicked
        });
        if clicked && !open {
            self.search = Some(String::new());
        }
        if let Some(message) = &self.message {
            bar.child()
                .insert(Text::new(message).style(style(13.0, Color::rgb(255, 120, 110))));
        }
    }

    fn pane(&mut self, ui: Ui<'_>, side: Side) {
        let mut column = ui.layout(flex::column().align(Align::Stretch));
        column
            .child()
            .item(flex::item().width(Sizing::grow()))
            .build(|ui: Ui<'_>| self.pane_header(ui, side));
        column
            .child()
            .item(flex::item().grow())
            .build(|ui: Ui<'_>| self.canvas(ui, side));
    }

    fn pane_header(&mut self, ui: Ui<'_>, side: Side) {
        let mut bar = ui.layout(flex::row().padding(Sides::xy(10.0, 5.0)).gap(8.0).align(Align::Center));
        bar.insert(Rectangle::new().background(colors::PANEL));
        let sheets = self.design.sheets.len();
        match side {
            Side::Sch => {
                let title = match &self.design.sch_path {
                    Some(_) => "Schematic",
                    None => "Schematic (no .sch)",
                };
                bar.child()
                    .insert(Text::new(title).style(style(13.0, colors::TEXT).weight(600)));
                if sheets > 1 {
                    if chip(&mut bar, "◀", self.sheet > 0) {
                        self.set_sheet(self.sheet.saturating_sub(1));
                    }
                    let label = format!("sheet {}/{}", self.sheet + 1, sheets);
                    bar.child().insert(Text::new(&label).style(style(13.0, colors::MUTED)));
                    if chip(&mut bar, "▶", self.sheet + 1 < sheets) {
                        self.set_sheet(self.sheet + 1);
                    }
                }
            }
            Side::Brd => {
                let title = match &self.design.brd_path {
                    Some(_) => "Board",
                    None => "Board (no .brd)",
                };
                bar.child()
                    .insert(Text::new(title).style(style(13.0, colors::TEXT).weight(600)));
                if let Some(board) = &self.design.board {
                    let size = format!("{:.1} × {:.1} mm", board.bounds.width(), board.bounds.height());
                    bar.child().insert(Text::new(&size).style(style(13.0, colors::MUTED)));
                }
            }
        }
        bar.child()
            .item(flex::item().width(Sizing::grow()))
            .build(|_: Ui<'_>| {});
        if self.hover.0 == side
            && let Some(name) = self.design.selection_name(self.hover.1)
        {
            let hover = name.to_string();
            bar.child().insert(Text::new(&hover).style(style(13.0, colors::MUTED)));
        }
    }

    fn canvas(&mut self, ui: Ui<'_>, side: Side) {
        let id = Id::new(("canvas", side));
        let mut ui = ui.id(id).clip(BoundsClip);
        let area = ui.area(id);
        let press = ui.interact(interact::press());
        let drag = ui.interact(interact::drag());
        let scroll = ui.interact(interact::scroll());
        let input = ui.input();
        let pointer = ui.mouse_position();
        let scale = self.ui_scale();
        let Some(kind) = scene(&self.design, self.sheet, side).map(|s| s.kind) else {
            let mut node = ui.layout(flex::column().align(Align::Center).justify(Justify::Center));
            node.child()
                .insert(Text::new("nothing to show").style(style(14.0, colors::MUTED)));
            return;
        };
        let mut node = ui.layout(flex::column());
        node.insert(Rectangle::new().background(rgb(render::background(kind))));
        let Some(area) = area else {
            node.request_frame();
            return;
        };
        let size = (
            ((area.width * scale).round() as u32).max(1),
            ((area.height * scale).round() as u32).max(1),
        );
        let inside = pointer.is_some_and(|p| {
            p.x >= area.x && p.y >= area.y && p.x <= area.x + area.width && p.y <= area.y + area.height
        });
        let local = pointer.map(|p| (((p.x - area.x) * scale) as f64, ((p.y - area.y) * scale) as f64));

        // the view follows the canvas size and the gestures
        let sheet = self.sheet;
        let design = &self.design;
        let canvas = match side {
            Side::Sch => &mut self.sch,
            Side::Brd => &mut self.brd,
        };
        let scene = scene(design, sheet, side).expect("scene");
        let view = canvas
            .view
            .get_or_insert_with(|| Viewport::fit(scene.bounds, size.0, size.1));
        if (view.width, view.height) != size {
            view.width = size.0;
            view.height = size.1;
        }
        let s = scale as f64;
        if (drag.active || drag.released) && (drag.delta.x != 0.0 || drag.delta.y != 0.0) {
            view.pan(drag.delta.x as f64 * s, drag.delta.y as f64 * s);
        }
        if let Some((dx, dy)) = raw_drag(&mut self.raw_drag, side, input, inside) {
            view.pan(dx as f64 * s, dy as f64 * s);
        }
        if scroll.delta.x != 0.0 || scroll.delta.y != 0.0 {
            let control = matches!(input, Input::Scroll { modifiers, .. } if modifiers.control());
            if scroll.continuous && !control {
                view.pan(-scroll.delta.x as f64 * s, -scroll.delta.y as f64 * s);
            } else if let Some((x, y)) = local {
                view.zoom_at(1.2f64.powf(-scroll.delta.y as f64 / 40.0), x, y);
            }
        }
        // picking: a few pixels of tolerance
        let tolerance = 4.0 * s / view.scale;
        let pick = |x: f64, y: f64| scene.pick(view.to_world(x, y), tolerance);
        if matches!(input, Input::MouseMove { .. } | Input::MouseLeave) {
            match (inside, local) {
                (true, Some((x, y))) => self.hover = (side, pick(x, y)),
                _ if self.hover.0 == side => self.hover.1 = Selection::None,
                _ => {}
            }
        }
        let clicked = match (press.clicked, local) {
            (true, Some((x, y))) => Some(pick(x, y)),
            _ => None,
        };
        if let Some(selection) = clicked {
            let (px, py) = (pointer.map_or(0.0, |p| p.x), pointer.map_or(0.0, |p| p.y));
            let double = self.last_click.is_some_and(|(at, x, y)| {
                at.elapsed() < Duration::from_millis(450) && (x - px).abs() < 5.0 && (y - py).abs() < 5.0
            });
            self.last_click = if double { None } else { Some((Instant::now(), px, py)) };
            self.select(selection, Some(side), double);
        }
        let reveals: Vec<(Side, bool)> = self
            .pending_reveal
            .iter()
            .copied()
            .filter(|(s, _)| *s == side)
            .collect();
        if !reveals.is_empty() {
            self.pending_reveal.retain(|(s, _)| *s != side);
            for (_, zoom) in reveals {
                self.reveal(side, zoom);
            }
        }

        // render when what is shown changed
        let sheet = self.sheet;
        let selection = self.selection;
        let design = &self.design;
        let canvas = match side {
            Side::Sch => &mut self.sch,
            Side::Brd => &mut self.brd,
        };
        let Some(scene) = self::scene(design, sheet, side) else {
            return;
        };
        let view = canvas.view.expect("view");
        let stale = canvas
            .shown
            .as_ref()
            .is_none_or(|s| s.view != view || s.selection != selection || s.sheet != sheet);
        if stale {
            let pixmap = render::render(scene, &view, selection);
            let data = ImageData::new(
                ImagePixels::Owned(pixmap.take().into_boxed_slice()),
                ImageFormat::Rgba8Premultiplied,
                view.width as usize,
                view.height as usize,
            );
            match node.context_mut().create_image(data) {
                Ok(image) => {
                    canvas.shown = Some(Shown {
                        image,
                        view,
                        selection,
                        sheet,
                    })
                }
                Err(error) => eprintln!("[bligle] canvas image: {error}"),
            }
        }
        if let Some(shown) = &canvas.shown {
            node.insert(CanvasImage {
                image: shown.image.id(),
                width: shown.view.width as f32 / scale,
                height: shown.view.height as f32 / scale,
            });
        }
    }

    fn info_bar(&mut self, ui: Ui<'_>) {
        let mut bar = ui.layout(flex::row().padding(Sides::xy(12.0, 6.0)).gap(14.0).align(Align::Center));
        bar.insert(Rectangle::new().background(colors::PANEL));
        let d = &self.design;
        let mut fields: Vec<(String, Color)> = Vec::new();
        match self.selection {
            Selection::None => fields.push((
                "click: select  ·  double-click: zoom both  ·  drag / right-drag: pan  ·  wheel: zoom  ·  F: fit  ·  Z: zoom to selection  ·  /: find  ·  PgUp/PgDn: sheet  ·  Esc: clear".to_string(),
                colors::MUTED,
            )),
            Selection::Part(p) => {
                fields.push((d.parts[p as usize].name.clone(), rgb(HIGHLIGHT)));
                fields.push((d.value(p), colors::TEXT));
                if let Some(e) = d.element(p) {
                    fields.push((e.package.clone(), colors::MUTED));
                    let side = if e.rot.mirror { "bottom" } else { "top" };
                    fields.push((format!("({:.2}, {:.2}) mm {side} {}", e.at.x, e.at.y, e.rot.to_eagle()), colors::MUTED));
                } else if let Some(package) = d.sch_package(p) {
                    fields.push((format!("{package} (not on the board)"), colors::MUTED));
                }
                let sheets: Vec<String> = d.sch_locations(p).iter().map(|(s, _)| (s + 1).to_string()).collect();
                if !sheets.is_empty() {
                    fields.push((format!("sheet {}", sheets.join(", ")), colors::MUTED));
                }
                let description = d.description(p);
                if !description.is_empty() {
                    fields.push((description, colors::MUTED));
                }
            }
            Selection::Net(n) => {
                fields.push((d.nets[n as usize].name.clone(), rgb(HIGHLIGHT)));
                let pins = d.pins.iter().filter(|pin| pin.net == Some(n)).count();
                let pads = d.pads.iter().filter(|pad| pad.net == Some(n)).count();
                fields.push((format!("{pins} pins in the schematic, {pads} pads on the board"), colors::TEXT));
                let sheets: Vec<String> = d.net_sheets(n).iter().map(|s| (s + 1).to_string()).collect();
                if !sheets.is_empty() {
                    fields.push((format!("sheet {}", sheets.join(", ")), colors::MUTED));
                }
            }
        }
        for (text, color) in &fields {
            bar.child().insert(
                Text::new(text)
                    .style(style(13.0, *color))
                    .options(TextOptions::new().overflow(TextOverflow::Ellipsis)),
            );
        }
    }
}

/// a small button; returns whether it was clicked
fn chip(bar: &mut Ui<'_, blit::state::Open<flex::Layout>>, label: &str, enabled: bool) -> bool {
    bar.child().build(|mut ui: Ui<'_>| {
        let press = ui.interact(interact::press());
        let mut chip = ui.layout(flex::row().padding(Sides::xy(7.0, 2.0)));
        chip.insert(
            Rectangle::new()
                .background(if press.hovered && enabled {
                    colors::CHIP_HOVER
                } else {
                    colors::CHIP
                })
                .radius(BorderRadius::uniform(4.0)),
        );
        chip.child()
            .insert(Text::new(label).style(style(12.0, if enabled { colors::TEXT } else { colors::MUTED })));
        press.clicked && enabled
    })
}

/// right / middle button drags, decoded from the raw input
fn raw_drag(state: &mut Option<(Side, f32, f32)>, side: Side, input: Input<f32>, inside: bool) -> Option<(f32, f32)> {
    match input {
        Input::MouseDown { position, button, .. }
            if inside && matches!(button, MouseButton::Secondary | MouseButton::Middle) =>
        {
            *state = Some((side, position.x, position.y));
            None
        }
        Input::MouseMove { position, .. } => {
            let (owner, x, y) = (*state)?;
            if owner != side {
                return None;
            }
            *state = Some((side, position.x, position.y));
            Some((position.x - x, position.y - y))
        }
        Input::MouseUp {
            button: MouseButton::Secondary | MouseButton::Middle,
            ..
        }
        | Input::MouseLeave
        | Input::Cancel => {
            *state = None;
            None
        }
        _ => None,
    }
}

/// paints a canvas image at the top left of its node (which clips it)
struct CanvasImage {
    image: ImageId,
    width: f32,
    height: f32,
}

impl<C: Context> Atom<C> for CanvasImage {
    fn measure(&self, _: &mut AtomCx<'_, C>, constraints: Constraints<f32>) -> Size<f32> {
        constraints.constrain(Size::ZERO)
    }

    fn paint(&self, cx: &mut AtomCx<'_, C>, area: LogicalRect) {
        cx.context_mut().as_mut().paint_image(ImageRequest {
            image: self.image,
            area: LogicalRect::new(area.x, area.y, self.width, self.height),
            fit: ImageFit::Fill,
            sampling: ImageSampling::Nearest,
            opacity: 1.0,
            colorize: None,
            nine_slice: None,
            horizontal_tiling: ImageTiling::None,
            vertical_tiling: ImageTiling::None,
        });
    }

    fn paint_bounds(&self, area: LogicalRect) -> LogicalRect {
        LogicalRect::new(area.x, area.y, self.width, self.height)
    }
}
