//! the viewer: the schematic and the board side by side, one selection shared by both,
//! and the list of recent projects to switch between

use std::{
    path::{Path, PathBuf},
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
    ScrollArea, ScrollState, Text, interact,
    layout::{Align, Justify, Sizing, flex},
};

use crate::{
    design::{Design, Kind, Scene, Selection},
    geom::{Rect, Viewport},
    render::{self, HIGHLIGHT, Rgb},
    sessions::{self, Entry, Session, Sessions, View},
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
    /// an empty design when no project is open
    pub design: Arc<Design>,
    /// the open project's file, its key in the sessions
    path: Option<PathBuf>,
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
    sessions: Sessions,
    /// the sheet and selection last written to the sessions
    saved: (usize, Selection),
    /// the project list, shown instead of the panes
    projects: Option<Projects>,
    /// folders opened from the project list's path field
    folders: Vec<PathBuf>,
}

struct Projects {
    entries: Vec<Entry>,
    /// a path being typed
    input: Option<String>,
    scroll: ScrollState,
}

/// the scene a side shows
fn scene(design: &Design, sheet: usize, side: Side) -> Option<&Scene> {
    match side {
        Side::Sch => design.sheets.get(sheet),
        Side::Brd => design.board.as_ref(),
    }
}

impl App {
    /// no project open yet: the project list shows
    pub fn new(scale: Arc<AtomicU32>, sessions: Sessions) -> App {
        let mut app = App {
            design: Arc::new(Design::new(String::new(), None, None, None, None)),
            path: None,
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
            sessions,
            saved: (0, Selection::None),
            projects: None,
            folders: Vec::new(),
        };
        app.show_projects();
        app
    }

    /// shows a design as it opens (the headless screenshots start there)
    pub fn set_design(&mut self, design: Arc<Design>) {
        self.path = sessions::key(&design);
        self.design = design;
        self.sheet = 0;
        self.selection = Selection::None;
        self.sch = Canvas::default();
        self.brd = Canvas::default();
        self.hover = (Side::Sch, Selection::None);
        self.search = None;
        self.message = None;
        self.last_click = None;
        self.raw_drag = None;
        self.pending_reveal.clear();
        self.saved = (0, Selection::None);
        self.projects = None;
    }

    /// opens a loaded project where it was left, and puts it first in the recent ones
    pub fn show(&mut self, design: Arc<Design>) {
        self.remember();
        self.set_design(design);
        let Some(path) = self.path.clone() else { return };
        if let Some(saved) = self.sessions.get(&path).cloned() {
            self.restore(&saved);
        }
        self.sessions.touch(&path);
        self.remember();
        self.sessions.save();
    }

    /// opens a project file, or lists the projects of a folder
    pub fn open(&mut self, path: &Path) {
        if path.is_dir() {
            let dir = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            self.folders.retain(|d| *d != dir);
            self.folders.insert(0, dir);
            self.show_projects();
            return;
        }
        match Design::open(path) {
            Ok(design) => self.show(Arc::new(design)),
            Err(error) => {
                self.message = Some(format!("{error:#}"));
                self.show_projects();
            }
        }
    }

    /// reopens the most recent project that still exists
    pub fn open_last(&mut self) {
        let last = self
            .sessions
            .list
            .iter()
            .find(|s| crate::design::project_files(&s.path).is_ok())
            .map(|s| s.path.clone());
        match last {
            Some(path) => self.open(&path),
            None => self.show_projects(),
        }
    }

    /// the window title
    pub fn title(&self) -> String {
        match self.path {
            Some(_) => format!("{} — bligle", self.design.name),
            None => "bligle".to_string(),
        }
    }

    fn show_projects(&mut self) {
        // a headless screenshot's project is not in the (read-only) list yet
        if let Some(path) = &self.path
            && self.sessions.get(path).is_none()
        {
            self.sessions.touch(&path.clone());
        }
        let entries = self.sessions.entries(&self.folders);
        match &mut self.projects {
            Some(projects) => projects.entries = entries,
            None => {
                self.projects = Some(Projects {
                    entries,
                    input: None,
                    scroll: ScrollState::new(),
                })
            }
        }
    }

    /// back to the viewer, when a project is open
    fn hide_projects(&mut self) {
        if self.path.is_some() {
            self.projects = None;
        }
    }

    /// stores the open project's sheet, selection and views in the sessions
    fn remember(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let selection = match self.selection {
            Selection::None => None,
            Selection::Part(p) => Some(format!("part:{}", self.design.parts[p as usize].name)),
            Selection::Net(n) => Some(format!("net:{}", self.design.nets[n as usize].name)),
        };
        let view = |canvas: &Canvas| {
            canvas.view.map(|v| View {
                center: v.center,
                scale: v.scale,
            })
        };
        let opened = self.sessions.get(&path).map_or_else(sessions::now, |s| s.opened);
        self.sessions.update(Session {
            path,
            opened,
            sheet: self.sheet,
            selection,
            views: [view(&self.sch), view(&self.brd)],
        });
        self.saved = (self.sheet, self.selection);
    }

    /// back to a saved sheet, selection and views
    fn restore(&mut self, saved: &Session) {
        if saved.sheet < self.design.sheets.len() {
            self.sheet = saved.sheet;
        }
        self.selection = match saved.selection.as_deref().and_then(|s| s.split_once(':')) {
            Some(("part", name)) => self.design.part_id(name).map_or(Selection::None, Selection::Part),
            Some(("net", name)) => self.design.net_id(name).map_or(Selection::None, Selection::Net),
            _ => Selection::None,
        };
        for (canvas, view) in [&mut self.sch, &mut self.brd].into_iter().zip(saved.views) {
            // the canvas sets the size on its next frame
            canvas.view = view.map(|v| Viewport {
                center: v.center,
                scale: v.scale,
                width: 1,
                height: 1,
            });
        }
        for (side, view) in [(Side::Sch, saved.views[0]), (Side::Brd, saved.views[1])] {
            if view.is_none() {
                self.pending_reveal.push((side, false));
            }
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
        // the sheet and selection are saved as they change (the views when leaving)
        if self.path.is_some() && self.saved != (self.sheet, self.selection) {
            self.remember();
            self.sessions.save();
        }
        let mut root = ui.layout(flex::column().align(Align::Stretch));
        root.insert(Rectangle::new().background(colors::BACKGROUND));
        root.child()
            .item(flex::item().width(Sizing::grow()))
            .build(|ui: Ui<'_>| self.top_bar(ui));
        root.child().item(flex::item().grow()).build(|ui: Ui<'_>| {
            if self.projects.is_some() {
                return self.projects_page(ui);
            }
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
        let toggle = matches!(
            input,
            Input::Key(KeyInput { key: Key::Character('o' | 'O'), pressed: true, modifiers, .. }) if modifiers.control()
        );
        if let Some(projects) = &mut self.projects {
            if let Some(path) = &mut projects.input {
                match input {
                    Input::Text(c) if !c.is_control() => path.push(c),
                    Input::Key(KeyInput { key, pressed: true, .. }) => match key {
                        Key::Backspace => {
                            path.pop();
                        }
                        Key::Escape => projects.input = None,
                        Key::Enter => {
                            let text = projects.input.take().unwrap_or_default();
                            if !text.trim().is_empty() {
                                self.message = None;
                                self.open(&sessions::expand(text.trim()));
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
                return;
            }
            match input {
                // a path starts with / or ~
                Input::Text(c @ ('/' | '~')) => {
                    projects.input = Some(c.to_string());
                    self.message = None;
                }
                Input::Text(c @ '1'..='9') => {
                    let n = c as usize - '1' as usize;
                    let recent = projects.entries.iter().filter(|e| e.opened.is_some()).nth(n);
                    if let Some(path) = recent.map(|e| e.path.clone()) {
                        self.open(&path);
                    }
                }
                Input::Key(KeyInput {
                    key: Key::Escape,
                    pressed: true,
                    ..
                }) => self.hide_projects(),
                _ if toggle => self.hide_projects(),
                _ => {}
            }
            return;
        }
        if toggle {
            self.search = None;
            self.show_projects();
            return;
        }
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
        // the project name opens the project list
        let listing = self.projects.is_some();
        let label = match self.path {
            Some(_) => format!("{}  ▾", self.design.name),
            None => "no project open".to_string(),
        };
        let toggled = bar.child().build(|mut ui: Ui<'_>| {
            let press = ui.interact(interact::press());
            let mut chip = ui.layout(flex::row().padding(Sides::xy(10.0, 4.0)));
            chip.insert(
                Rectangle::new()
                    .background(if listing || press.hovered {
                        colors::CHIP_HOVER
                    } else {
                        colors::PANEL
                    })
                    .radius(BorderRadius::uniform(4.0)),
            );
            chip.child().insert(Text::new(&label).style(style(14.0, colors::TEXT)));
            press.clicked
        });
        if toggled {
            if listing {
                self.hide_projects();
            } else {
                self.search = None;
                self.show_projects();
            }
        }
        bar.child()
            .item(flex::item().width(Sizing::grow()))
            .build(|_: Ui<'_>| {});
        if listing {
            if let Some(message) = &self.message {
                bar.child()
                    .insert(Text::new(message).style(style(13.0, Color::rgb(255, 120, 110))));
            }
            return;
        }
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

    /// the recent projects, then the others found in their folders
    fn projects_page(&mut self, ui: Ui<'_>) {
        let current = self.path.clone();
        let Some(Projects { entries, input, scroll }) = &mut self.projects else {
            return;
        };
        let mut page = ui.layout(
            flex::column()
                .padding(Sides::xy(28.0, 18.0))
                .gap(12.0)
                .align(Align::Stretch),
        );
        page.insert(Rectangle::new().background(colors::BACKGROUND));

        let typing = input.is_some();
        let field = match input {
            Some(text) => format!("open: {text}▏"),
            None => "/ or ~ to type a path: a .sch, a .brd, or a folder to list its projects".to_string(),
        };
        let clicked = page.child().build(|mut ui: Ui<'_>| {
            let press = ui.interact(interact::press());
            let mut row = ui.layout(flex::row().padding(Sides::xy(12.0, 7.0)));
            row.insert(
                Rectangle::new()
                    .background(if typing || press.hovered {
                        colors::CHIP_HOVER
                    } else {
                        colors::CHIP
                    })
                    .radius(BorderRadius::uniform(4.0)),
            );
            row.child()
                .insert(Text::new(&field).style(style(13.0, if typing { colors::TEXT } else { colors::MUTED })));
            press.clicked
        });
        if clicked && !typing {
            *input = Some(String::new());
        }

        let mut open: Option<PathBuf> = None;
        if entries.is_empty() {
            page.child().insert(
                Text::new(
                    "No project yet: type the path of an Eagle .sch or .brd above, or start with  bligle <project>",
                )
                .style(style(14.0, colors::MUTED)),
            );
        }
        let entries = &*entries;
        page.child()
            .item(flex::item().grow())
            .build(ScrollArea::new(scroll).clip(BoundsClip).build(|ui: Ui<'_>| {
                let mut list = ui.layout(flex::column().gap(4.0).align(Align::Stretch));
                let recent = entries.iter().filter(|e| e.opened.is_some()).count();
                for (i, entry) in entries.iter().enumerate() {
                    if i == 0 && recent > 0 {
                        list.child()
                            .insert(Text::new("Recent").style(style(13.0, colors::MUTED).weight(600)));
                    }
                    if i == recent {
                        list.child()
                            .item(flex::item().height(Sizing::fixed(8.0)))
                            .build(|_: Ui<'_>| {});
                        list.child()
                            .insert(Text::new("In the same folders").style(style(13.0, colors::MUTED).weight(600)));
                    }
                    let is_current = current.as_ref() == Some(&entry.path);
                    let key = if i < recent.min(9) {
                        (i + 1).to_string()
                    } else {
                        String::new()
                    };
                    if list
                        .child()
                        .build(|ui: Ui<'_>| project_row(ui, entry, &key, is_current))
                    {
                        open = Some(entry.path.clone());
                    }
                }
            }));
        if let Some(path) = open {
            self.message = None;
            if current.as_ref() == Some(&path) {
                self.hide_projects();
            } else {
                self.open(&path);
            }
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
            _ if self.projects.is_some() => fields.push((
                if self.path.is_some() {
                    "click or 1–9: open a project  ·  / or ~: type a path  ·  Enter: open it  ·  Ctrl+O / Esc: back to the viewer"
                } else {
                    "click or 1–9: open a project  ·  / or ~: type a path  ·  Enter: open it"
                }
                .to_string(),
                colors::MUTED,
            )),
            Selection::None => fields.push((
                "click: select  ·  double-click: zoom both  ·  drag / right-drag: pan  ·  wheel: zoom  ·  F: fit  ·  Z: zoom to selection  ·  /: find  ·  PgUp/PgDn: sheet  ·  Ctrl+O: projects  ·  Esc: clear".to_string(),
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

impl Drop for App {
    /// the views are saved when leaving a project, and so when the window closes
    fn drop(&mut self) {
        if self.path.is_some() {
            self.remember();
            self.sessions.save();
        }
    }
}

/// one project in the list; returns whether it was clicked
fn project_row(mut ui: Ui<'_>, entry: &Entry, key: &str, current: bool) -> bool {
    let press = ui.interact(interact::press());
    let mut row = ui.layout(flex::row().padding(Sides::xy(12.0, 8.0)).gap(14.0).align(Align::Center));
    row.insert(
        Rectangle::new()
            .background(if press.hovered {
                colors::CHIP_HOVER
            } else {
                colors::PANEL
            })
            .radius(BorderRadius::uniform(5.0)),
    );
    let one_line = || TextOptions::new().overflow(TextOverflow::Ellipsis);
    row.child()
        .item(flex::item().width(Sizing::fixed(14.0)))
        .insert(Text::new(key).style(style(13.0, colors::MUTED)));
    row.child().item(flex::item().width(Sizing::fixed(300.0))).insert(
        Text::new(&entry.name)
            .style(style(14.0, if current { rgb(HIGHLIGHT) } else { colors::TEXT }).weight(600))
            .options(one_line()),
    );
    row.child().item(flex::item().grow()).insert(
        Text::new(&entry.folder)
            .style(style(13.0, colors::MUTED))
            .options(one_line()),
    );
    let files = match (entry.sch, entry.brd) {
        (true, true) => "sch + brd",
        (true, false) => "sch",
        _ => "brd",
    };
    row.child()
        .item(flex::item().width(Sizing::fixed(70.0)))
        .insert(Text::new(files).style(style(12.0, colors::MUTED)));
    let when = match (current, entry.opened) {
        (true, _) => "open now".to_string(),
        (false, Some(opened)) => sessions::ago(opened),
        (false, None) => String::new(),
    };
    row.child()
        .item(flex::item().width(Sizing::fixed(90.0)))
        .insert(Text::new(&when).style(style(12.0, if current { rgb(HIGHLIGHT) } else { colors::MUTED })));
    press.clicked
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    /// each project keeps its selection and views across switches, and the last one
    /// reopens with the next start
    #[test]
    fn sessions_follow_the_viewer() {
        // the sample projects live next to the repository, not in it
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Projects/Leroy");
        let a = dir.join("DataplugEmulator - revA0.sch");
        let b = dir.join("GPTechinno - RIOM-ANA-Test - revA2.sch");
        if !a.exists() || !b.exists() {
            return;
        }
        let file = std::env::temp_dir().join(format!("bligle-sessions-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let scale = || Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let view = Viewport {
            center: Point::new(12.5, -3.0),
            scale: 9.0,
            width: 100,
            height: 80,
        };
        {
            let mut app = App::new(scale(), Sessions::from_file(Some(file.clone()), true));
            assert!(app.projects.is_some(), "the project list shows first");
            app.open(&a);
            assert!(app.projects.is_none());
            assert!(app.select_named("AVR_MISO"));
            app.brd.view = Some(view);
            app.open(&b);
            assert_eq!(app.selection, Selection::None);
            app.open(&a);
            assert_eq!(app.design.selection_name(app.selection), Some("AVR_MISO"));
            assert_eq!(
                app.brd.view.map(|v| (v.center, v.scale)),
                Some((view.center, view.scale))
            );
            app.open(&b);
        }
        let mut app = App::new(scale(), Sessions::from_file(Some(file.clone()), true));
        app.open_last();
        assert_eq!(app.title(), "GPTechinno - RIOM-ANA-Test - revA2 — bligle");
        app.show_projects();
        let entries = &app.projects.as_ref().unwrap().entries;
        let recent: Vec<&str> = entries
            .iter()
            .filter(|e| e.opened.is_some())
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            recent,
            ["GPTechinno - RIOM-ANA-Test - revA2", "DataplugEmulator - revA0"]
        );
        assert!(
            entries
                .iter()
                .any(|e| e.opened.is_none() && e.name == "GPTechinno - FoxTetrixInterface - revA2")
        );
        let _ = std::fs::remove_file(&file);
    }
}
