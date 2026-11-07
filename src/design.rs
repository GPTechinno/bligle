//! a design: the schematic and the board of one Eagle project, matched part by part
//! (Eagle keeps the same names on both sides) and net by net, and flattened into
//! scenes of world-space items tagged with the part and net they belong to

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    eagle::{self, Align, Attr, Board, Display, PadShape, PinFunction, Prim, Restring, Schematic},
    geom::{Point, Rect, Rot, Xform, arc_points, segment_dist},
    text::{Placed, strip_overbar},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Selection {
    #[default]
    None,
    Part(u32),
    Net(u32),
}

impl Selection {
    pub fn matches(self, item: &Item) -> bool {
        match self {
            Selection::None => false,
            Selection::Part(p) => item.part == Some(p),
            Selection::Net(n) => item.net == Some(n),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Schematic,
    Board,
}

#[derive(Clone, Debug)]
pub enum Shape {
    /// an open polyline: a wire, a track or an arc
    Line { pts: Vec<Point>, width: f64, flat: bool },
    /// a circle, or a disc when `width` is 0
    Circle { c: Point, r: f64, width: f64 },
    /// a closed polygon; a pour is a signal polygon (Eagle stores its outline only)
    Poly { pts: Vec<Point>, width: f64, pour: bool },
    Text(Placed),
    /// a hole through the board
    Drill { c: Point, r: f64 },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub shape: Shape,
    pub layer: u16,
    pub part: Option<u32>,
    pub net: Option<u32>,
    pub bounds: Rect,
}

impl Item {
    /// distance from `p` to what this item covers
    fn distance(&self, p: Point) -> f64 {
        match &self.shape {
            Shape::Line { pts, width, .. } => {
                let d = pts.windows(2).map(|w| segment_dist(p, w[0], w[1])).fold(f64::INFINITY, f64::min);
                (d - width * 0.5).max(0.0)
            }
            Shape::Circle { c, r, width } => {
                let d = p.dist(*c);
                if *width == 0.0 { (d - r).max(0.0) } else { ((d - r).abs() - width * 0.5).max(0.0) }
            }
            Shape::Poly { pts, width, pour } => {
                let edge = pts
                    .iter()
                    .zip(pts.iter().cycle().skip(1))
                    .map(|(a, b)| segment_dist(p, *a, *b))
                    .fold(f64::INFINITY, f64::min);
                if !pour && inside(pts, p) { 0.0 } else { (edge - width * 0.5).max(0.0) }
            }
            Shape::Text(_) => {
                if self.bounds.contains(p) {
                    0.0
                } else {
                    f64::INFINITY
                }
            }
            Shape::Drill { c, r } => (p.dist(*c) - r).max(0.0),
        }
    }
}

fn inside(pts: &[Point], p: Point) -> bool {
    let mut inside = false;
    let mut j = pts.len().wrapping_sub(1);
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// whether a layer is drawn (and picked)
pub fn shown(kind: Kind, layer: u16) -> bool {
    match kind {
        Kind::Schematic => true,
        Kind::Board => matches!(layer, 1..=22 | 25..=28 | 44..=46 | 51 | 100..),
    }
}

pub struct Scene {
    pub kind: Kind,
    /// in drawing order
    pub items: Vec<Item>,
    pub bounds: Rect,
    /// where each part is (its drawing, without its texts)
    pub part_bounds: HashMap<u32, Rect>,
    /// Eagle colour index of each layer
    pub layer_colors: HashMap<u16, u8>,
}

impl Scene {
    /// what is under the world point `p`: a net's wire, track or via first, else the
    /// smallest part around it
    pub fn pick(&self, p: Point, tolerance: f64) -> Selection {
        let mut best: Option<(f64, u32)> = None;
        for item in &self.items {
            let (None, Some(net)) = (item.part, item.net) else { continue };
            if !shown(self.kind, item.layer) || !item.bounds.inflate(tolerance).contains(p) {
                continue;
            }
            let d = item.distance(p);
            if d <= tolerance && best.is_none_or(|(b, _)| d < b) {
                best = Some((d, net));
            }
        }
        if let Some((_, net)) = best {
            return Selection::Net(net);
        }
        self.part_bounds
            .iter()
            .filter(|(_, r)| r.inflate(tolerance).contains(p))
            .min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
            .map_or(Selection::None, |(&part, _)| Selection::Part(part))
    }

    /// what the selection covers in this scene
    pub fn selection_bounds(&self, selection: Selection) -> Rect {
        let mut rect = Rect::EMPTY;
        match selection {
            Selection::None => {}
            Selection::Part(p) => {
                if let Some(r) = self.part_bounds.get(&p) {
                    rect = *r;
                }
            }
            Selection::Net(_) => {
                for item in self.items.iter().filter(|i| selection.matches(i) && shown(self.kind, i.layer)) {
                    rect.union(item.bounds);
                }
            }
        }
        rect
    }
}

pub struct PartRef {
    pub name: String,
    /// index in `Schematic::parts`
    pub sch: Option<usize>,
    /// index in `Board::elements`
    pub brd: Option<usize>,
}

pub struct NetRef {
    pub name: String,
    pub in_sch: bool,
    pub in_brd: bool,
}

/// a pin of a placed gate in the schematic
pub struct PinInfo {
    pub part: u32,
    pub gate: String,
    pub pin: String,
    /// the package pads it connects to
    pub pads: Vec<String>,
    pub sheet: usize,
    pub at: Point,
    pub net: Option<u32>,
}

/// a pad of a placed package on the board
pub struct PadInfo {
    pub part: u32,
    pub pad: String,
    pub at: Point,
    pub smd: bool,
    pub bottom: bool,
    pub net: Option<u32>,
}

pub struct Design {
    pub name: String,
    pub sch_path: Option<PathBuf>,
    pub brd_path: Option<PathBuf>,
    pub sch: Option<Schematic>,
    pub brd: Option<Board>,
    /// sorted by name
    pub parts: Vec<PartRef>,
    part_ids: HashMap<String, u32>,
    /// sorted by name
    pub nets: Vec<NetRef>,
    net_ids: HashMap<String, u32>,
    pub sheets: Vec<Scene>,
    pub board: Option<Scene>,
    pub pins: Vec<PinInfo>,
    pub pads: Vec<PadInfo>,
}

/// the .sch and .brd of a project from either of them (or their common stem)
pub fn project_files(path: &Path) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    let sibling = |ext: &str| {
        let p = path.with_extension(ext);
        p.is_file().then_some(p)
    };
    let (sch, brd) = match ext.as_deref() {
        Some("sch") if path.is_file() => (Some(path.to_path_buf()), sibling("brd")),
        Some("brd") if path.is_file() => (sibling("sch"), Some(path.to_path_buf())),
        _ => {
            // a stem: Project -> Project.sch + Project.brd
            let with = |ext: &str| {
                let mut p = path.as_os_str().to_owned();
                p.push(format!(".{ext}"));
                let p = PathBuf::from(p);
                p.is_file().then_some(p)
            };
            (with("sch"), with("brd"))
        }
    };
    if sch.is_none() && brd.is_none() {
        bail!("{}: no Eagle .sch or .brd here", path.display());
    }
    Ok((sch, brd))
}

impl Design {
    pub fn open(path: &Path) -> Result<Design> {
        let (sch_path, brd_path) = project_files(path)?;
        let sch = sch_path.as_deref().map(eagle::parse_schematic).transpose()?;
        let brd = brd_path.as_deref().map(eagle::parse_board).transpose()?;
        let name = sch_path
            .as_ref()
            .or(brd_path.as_ref())
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .context("file name")?;
        Ok(Design::new(name, sch_path, brd_path, sch, brd))
    }

    pub fn new(
        name: String,
        sch_path: Option<PathBuf>,
        brd_path: Option<PathBuf>,
        sch: Option<Schematic>,
        brd: Option<Board>,
    ) -> Design {
        // parts and nets of both sides, by name
        let mut parts: Vec<PartRef> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        let mut part = |name: &str| -> usize {
            *by_name.entry(name.to_string()).or_insert_with(|| {
                parts.push(PartRef {
                    name: name.to_string(),
                    sch: None,
                    brd: None,
                });
                parts.len() - 1
            })
        };
        let mut slots = Vec::new();
        for (i, p) in sch.iter().flat_map(|s| s.parts.iter()).enumerate() {
            slots.push((part(&p.name), Some(i), None));
        }
        for (i, e) in brd.iter().flat_map(|b| b.elements.iter()).enumerate() {
            slots.push((part(&e.name), None, Some(i)));
        }
        for (slot, s, b) in slots {
            parts[slot].sch = parts[slot].sch.or(s);
            parts[slot].brd = parts[slot].brd.or(b);
        }
        parts.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        let part_ids = parts.iter().enumerate().map(|(i, p)| (p.name.clone(), i as u32)).collect();

        let mut net_names: HashMap<String, (bool, bool)> = HashMap::new();
        for net in sch.iter().flat_map(|s| s.sheets.iter()).flat_map(|s| s.nets.iter()) {
            net_names.entry(net.name.clone()).or_default().0 = true;
        }
        for signal in brd.iter().flat_map(|b| b.signals.iter()) {
            net_names.entry(signal.name.clone()).or_default().1 = true;
        }
        let mut nets: Vec<NetRef> = net_names
            .into_iter()
            .map(|(name, (in_sch, in_brd))| NetRef { name, in_sch, in_brd })
            .collect();
        nets.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        let net_ids = nets.iter().enumerate().map(|(i, n)| (n.name.clone(), i as u32)).collect();

        let mut design = Design {
            name,
            sch_path,
            brd_path,
            sch,
            brd,
            parts,
            part_ids,
            nets,
            net_ids,
            sheets: Vec::new(),
            board: None,
            pins: Vec::new(),
            pads: Vec::new(),
        };
        design.sheets = design.build_sheets();
        design.board = design.build_board();
        design
    }

    pub fn part_id(&self, name: &str) -> Option<u32> {
        self.part_ids.get(name).copied()
    }

    pub fn net_id(&self, name: &str) -> Option<u32> {
        self.net_ids.get(name).copied()
    }

    /// a part or a net by name, ignoring case
    pub fn find(&self, name: &str) -> Selection {
        let name = name.trim();
        if let Some(p) = self.part_id(name) {
            return Selection::Part(p);
        }
        if let Some(n) = self.net_id(name) {
            return Selection::Net(n);
        }
        if let Some(p) = self.parts.iter().position(|p| p.name.eq_ignore_ascii_case(name)) {
            return Selection::Part(p as u32);
        }
        if let Some(n) = self.nets.iter().position(|n| n.name.eq_ignore_ascii_case(name)) {
            return Selection::Net(n as u32);
        }
        Selection::None
    }

    pub fn selection_name(&self, selection: Selection) -> Option<&str> {
        match selection {
            Selection::None => None,
            Selection::Part(p) => Some(&self.parts[p as usize].name),
            Selection::Net(n) => Some(&self.nets[n as usize].name),
        }
    }

    pub fn sch_part(&self, part: u32) -> Option<&eagle::Part> {
        Some(&self.sch.as_ref()?.parts[self.parts[part as usize].sch?])
    }

    pub fn element(&self, part: u32) -> Option<&eagle::Element> {
        Some(&self.brd.as_ref()?.elements[self.parts[part as usize].brd?])
    }

    fn deviceset(&self, part: &eagle::Part) -> Option<&eagle::DeviceSet> {
        self.sch.as_ref()?.libraries.get(&part.library)?.devicesets.get(&part.deviceset)
    }

    /// the package the schematic's device uses (none for supply symbols, frames...)
    pub fn sch_package(&self, part: u32) -> Option<&str> {
        let p = self.sch_part(part)?;
        self.deviceset(p)?.devices.get(&p.device)?.package.as_deref()
    }

    /// the value shown for a part: its own, else what Eagle derives from the device
    pub fn value(&self, part: u32) -> String {
        if let Some(p) = self.sch_part(part) {
            return sch_value(p);
        }
        self.element(part).map(|e| e.value.clone()).unwrap_or_default()
    }

    pub fn description(&self, part: u32) -> String {
        let from_sch = self.sch_part(part).and_then(|p| self.deviceset(p)).map(|d| d.description.clone());
        from_sch.filter(|d| !d.is_empty()).unwrap_or_default()
    }

    /// the attributes of a part: its technology's, then its own, then the board's
    pub fn attributes(&self, part: u32) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut set = |k: &str, v: &str| {
            if v.is_empty() {
                return;
            }
            match out.iter_mut().find(|(key, _)| key == k) {
                Some(slot) => slot.1 = v.to_string(),
                None => out.push((k.to_string(), v.to_string())),
            }
        };
        if let Some(p) = self.sch_part(part) {
            if let Some(device) = self.deviceset(p).and_then(|d| d.devices.get(&p.device))
                && let Some(attrs) = device.technologies.get(&p.technology)
            {
                for (k, v) in attrs {
                    set(k, v);
                }
            }
            for (k, v) in &p.attributes {
                set(k, v);
            }
        }
        if let Some(e) = self.element(part) {
            for a in &e.attrs {
                if let Some(v) = &a.value {
                    set(&a.name, v);
                }
            }
        }
        out
    }

    /// sheets (0 based) and areas where a part's gates are
    pub fn sch_locations(&self, part: u32) -> Vec<(usize, Rect)> {
        self.sheets
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.part_bounds.get(&part).map(|r| (i, *r)))
            .collect()
    }

    /// sheets where a net has wires or pins
    pub fn net_sheets(&self, net: u32) -> Vec<usize> {
        self.sheets
            .iter()
            .enumerate()
            .filter(|(_, s)| s.items.iter().any(|i| i.net == Some(net)))
            .map(|(i, _)| i)
            .collect()
    }

    // --------------------------------------------------------------- scenes

    fn build_sheets(&mut self) -> Vec<Scene> {
        let Some(sch) = &self.sch else { return Vec::new() };
        // net of each (part, gate, pin)
        let mut pin_nets: HashMap<(&str, &str, &str), u32> = HashMap::new();
        for net in sch.sheets.iter().flat_map(|s| s.nets.iter()) {
            let id = self.net_ids[&net.name];
            for r in net.segments.iter().flat_map(|s| s.pinrefs.iter()) {
                pin_nets.insert((&r.part, &r.gate, &r.pin), id);
            }
        }
        let parts: HashMap<&str, &eagle::Part> = sch.parts.iter().map(|p| (p.name.as_str(), p)).collect();
        let sheet_count = sch.sheets.len();
        let mut scenes = Vec::new();
        let mut pins = Vec::new();
        for (index, sheet) in sch.sheets.iter().enumerate() {
            let mut b = Builder::new(Kind::Schematic, Restring::default());
            let globals = |t: &str| -> Option<String> {
                match t {
                    ">SHEET" => Some(format!("{}/{}", index + 1, sheet_count)),
                    ">SHEETNR" => Some((index + 1).to_string()),
                    ">SHEETS" => Some(sheet_count.to_string()),
                    ">DRAWING_NAME" => Some(self.name.clone()),
                    _ => lookup(t, &sch.attributes).or_else(|| literal(t)),
                }
            };
            for prim in &sheet.plain {
                b.flatten(prim, &Ctx::plain(), &globals);
            }
            for inst in &sheet.instances {
                let (Some(part), Some(&pid)) = (parts.get(inst.part.as_str()), self.part_ids.get(&inst.part)) else {
                    continue;
                };
                let Some(lib) = sch.libraries.get(&part.library) else { continue };
                let Some(ds) = lib.devicesets.get(&part.deviceset) else { continue };
                let Some(gate) = ds.gates.iter().find(|g| g.name == inst.gate) else { continue };
                let Some(symbol) = lib.symbols.get(&gate.symbol) else { continue };
                let device = ds.devices.get(&part.device);
                let mut lookup_pins = PinLookup::default();
                for c in device.iter().flat_map(|d| d.connects.iter()).filter(|c| c.gate == inst.gate) {
                    lookup_pins.pads.insert(c.pin.clone(), c.pads.clone());
                }
                for (&(p, g, pin), &net) in &pin_nets {
                    if p == part.name && g == inst.gate {
                        lookup_pins.nets.insert(pin.to_string(), net);
                    }
                }
                let shown_name = if ds.gates.len() > 1 {
                    format!("{}{}", part.name, inst.gate)
                } else {
                    part.name.clone()
                };
                let value = sch_value(part);
                let mut attrs = device
                    .and_then(|d| d.technologies.get(&part.technology))
                    .cloned()
                    .unwrap_or_default();
                attrs.extend(part.attributes.iter().cloned());
                let subst = |t: &str| -> Option<String> {
                    match t {
                        ">NAME" => (!inst.smashed).then(|| shown_name.clone()),
                        ">VALUE" => (!inst.smashed).then(|| value.clone()),
                        ">PART" => Some(part.name.clone()),
                        ">GATE" => Some(inst.gate.clone()),
                        _ => lookup(t, &attrs).or_else(|| globals(t)),
                    }
                };
                let cx = Ctx {
                    xform: Xform::place(inst.at, inst.rot),
                    part: Some(pid),
                    pins: Some(&lookup_pins),
                    gate: &inst.gate,
                    sheet: index,
                    ..Ctx::plain()
                };
                for prim in symbol {
                    b.flatten(prim, &cx, &subst);
                }
                for a in &inst.attrs {
                    let value = match a.name.as_str() {
                        "NAME" => Some(shown_name.clone()),
                        "VALUE" => Some(value.clone()),
                        "PART" => Some(part.name.clone()),
                        "GATE" => Some(inst.gate.clone()),
                        name => attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
                    };
                    b.attribute(a, value, pid);
                }
            }
            for net in &sheet.nets {
                let cx = Ctx {
                    net: self.net_ids.get(&net.name).copied(),
                    net_name: Some(&net.name),
                    ..Ctx::plain()
                };
                for prim in net.segments.iter().flat_map(|s| s.prims.iter()) {
                    b.flatten(prim, &cx, &globals);
                }
            }
            for bus in &sheet.busses {
                let cx = Ctx {
                    net_name: Some(&bus.name),
                    ..Ctx::plain()
                };
                for prim in bus.segments.iter().flat_map(|s| s.prims.iter()) {
                    b.flatten(prim, &cx, &globals);
                }
            }
            pins.append(&mut b.pins);
            scenes.push(b.finish(&sch.layers));
        }
        self.pins = pins;
        scenes
    }

    fn build_board(&mut self) -> Option<Scene> {
        let brd = self.brd.as_ref()?;
        let mut b = Builder::new(Kind::Board, brd.restring);
        // the board has no attributes of its own: the schematic's apply (Eagle shares them)
        let sch_attributes = self.sch.as_ref().map(|s| s.attributes.as_slice()).unwrap_or_default();
        let globals = |t: &str| -> Option<String> {
            match t {
                ">DRAWING_NAME" => Some(self.name.clone()),
                _ => lookup(t, sch_attributes).or_else(|| literal(t)),
            }
        };
        for prim in &brd.plain {
            b.flatten(prim, &Ctx::plain(), &globals);
        }
        let mut pad_nets: HashMap<&str, HashMap<String, u32>> = HashMap::new();
        for signal in &brd.signals {
            let id = self.net_ids[&signal.name];
            for (element, pad) in &signal.contacts {
                pad_nets.entry(element).or_default().insert(pad.clone(), id);
            }
        }
        let empty = HashMap::new();
        for e in &brd.elements {
            let Some(&pid) = self.part_ids.get(&e.name) else { continue };
            let Some(package) = brd.libraries.get(&e.library).and_then(|l| l.packages.get(&e.package)) else {
                continue;
            };
            let attrs: Vec<(String, String)> = e
                .attrs
                .iter()
                .filter_map(|a| Some((a.name.clone(), a.value.clone()?)))
                .collect();
            let subst = |t: &str| -> Option<String> {
                match t {
                    ">NAME" => (!e.smashed).then(|| e.name.clone()),
                    ">VALUE" => (!e.smashed).then(|| e.value.clone()),
                    _ => lookup(t, &attrs).or_else(|| globals(t)),
                }
            };
            let cx = Ctx {
                xform: Xform::place(e.at, e.rot),
                part: Some(pid),
                pad_nets: Some(pad_nets.get(e.name.as_str()).unwrap_or(&empty)),
                flip: e.rot.mirror,
                ..Ctx::plain()
            };
            for prim in &package.prims {
                b.flatten(prim, &cx, &subst);
            }
            for a in &e.attrs {
                let value = match a.name.as_str() {
                    "NAME" => Some(e.name.clone()),
                    "VALUE" => Some(e.value.clone()),
                    _ => a.value.clone(),
                };
                b.attribute(a, value, pid);
            }
        }
        for signal in &brd.signals {
            let cx = Ctx {
                net: self.net_ids.get(&signal.name).copied(),
                net_name: Some(&signal.name),
                ..Ctx::plain()
            };
            for prim in &signal.prims {
                b.flatten(prim, &cx, &globals);
            }
        }
        self.pads = std::mem::take(&mut b.pads);
        Some(b.finish(&brd.layers))
    }
}

/// the value Eagle shows for a schematic part
pub fn sch_value(part: &eagle::Part) -> String {
    if let Some(v) = &part.value {
        return v.clone();
    }
    let name = part.deviceset.replace('*', &part.technology);
    if name.contains('?') {
        name.replace('?', &part.device)
    } else {
        format!("{name}{}", part.device)
    }
}

/// a text as drawn when nothing replaces it: placeholders that nothing fills (like
/// `>LAST_DATE_TIME`) are left out rather than shown raw
fn literal(text: &str) -> Option<String> {
    (!text.starts_with('>')).then(|| text.to_string())
}

/// `>ATTR` placeholders from a list of attributes
fn lookup(text: &str, attrs: &[(String, String)]) -> Option<String> {
    let key = text.strip_prefix('>')?;
    attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.clone())
}

/// compares names so that R2 comes before R10
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> (String, u64, String) {
        let digits_at = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
        let rest = &s[digits_at..];
        let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        (
            s[..digits_at].to_ascii_uppercase(),
            rest[..digits_end].parse().unwrap_or(0),
            rest[digits_end..].to_string(),
        )
    };
    split(a).cmp(&split(b)).then_with(|| a.cmp(b))
}

/// the layer an element's primitive goes to when the element sits on the bottom
fn flip_layer(layer: u16) -> u16 {
    match layer {
        1 => 16,
        16 => 1,
        21..=42 if layer % 2 == 1 => layer + 1,
        21..=42 => layer - 1,
        51 => 52,
        52 => 51,
        _ => layer,
    }
}

/// drawing order of the layers
fn rank(kind: Kind, layer: u16) -> u16 {
    match kind {
        Kind::Schematic => match layer {
            98 => 0,
            91 | 92 => 1,
            94 => 2,
            _ => 3,
        },
        Kind::Board => match layer {
            22 | 24 | 26 | 28 | 30 | 32 | 34 | 36 | 38 | 40 | 42 | 52 => 0,
            16 => 10,
            2..=15 => 20,
            1 => 30,
            17 | 18 => 40,
            44 | 45 => 41,
            19 => 45,
            20 | 46..=50 => 46,
            _ => 50,
        },
    }
}

#[derive(Default)]
struct PinLookup {
    pads: HashMap<String, Vec<String>>,
    nets: HashMap<String, u32>,
}

/// how the primitives of a symbol, package or net are placed and tagged
struct Ctx<'a> {
    xform: Xform,
    part: Option<u32>,
    net: Option<u32>,
    net_name: Option<&'a str>,
    pins: Option<&'a PinLookup>,
    pad_nets: Option<&'a HashMap<String, u32>>,
    gate: &'a str,
    sheet: usize,
    /// the element is on the bottom side
    flip: bool,
}

impl Ctx<'_> {
    fn plain() -> Self {
        Ctx {
            xform: Xform::IDENTITY,
            part: None,
            net: None,
            net_name: None,
            pins: None,
            pad_nets: None,
            gate: "",
            sheet: 0,
            flip: false,
        }
    }
}

struct Builder {
    kind: Kind,
    restring: Restring,
    items: Vec<Item>,
    pins: Vec<PinInfo>,
    pads: Vec<PadInfo>,
}

/// Eagle's line width for pins
const PIN_WIDTH: f64 = 0.1524;

impl Builder {
    fn new(kind: Kind, restring: Restring) -> Self {
        Builder {
            kind,
            restring,
            items: Vec::new(),
            pins: Vec::new(),
            pads: Vec::new(),
        }
    }

    fn push(&mut self, shape: Shape, layer: u16, part: Option<u32>, net: Option<u32>) {
        let bounds = match &shape {
            Shape::Line { pts, width, .. } => bounds_of(pts).inflate(width * 0.5),
            Shape::Circle { c, r, width } => Rect::around(*c, r + width * 0.5),
            Shape::Poly { pts, width, .. } => bounds_of(pts).inflate(width * 0.5),
            Shape::Text(t) => bounds_of(&t.corners()),
            Shape::Drill { c, r } => Rect::around(*c, *r),
        };
        self.items.push(Item {
            shape,
            layer,
            part,
            net,
            bounds,
        });
    }

    fn text(&mut self, placed: Placed, layer: u16, part: Option<u32>, net: Option<u32>) {
        if !placed.text.trim().is_empty() && placed.size > 0.0 {
            self.push(Shape::Text(placed), layer, part, net);
        }
    }

    /// a text with an orientation composed through `xform`; schematic texts are never
    /// drawn mirrored (Eagle swaps their alignment instead)
    #[allow(clippy::too_many_arguments)]
    fn placed(&self, at: Point, text: &str, size: f64, xform: Xform, spin: bool, mut align: Align, distance: f64) -> Placed {
        let mut xform = xform;
        if self.kind == Kind::Schematic && xform.mirrored() {
            align.h = -align.h;
            xform = Xform::place(Point::default(), Rot { angle: xform.angle(), ..Rot::default() });
        }
        Placed::new(at, strip_overbar(text), size, xform, spin, align, distance)
    }

    /// a smashed attribute (placed on its own, in absolute coordinates)
    fn attribute(&mut self, a: &Attr, value: Option<String>, part: u32) {
        let (Some(at), Some(value)) = (a.at, value) else { return };
        let text = match a.display {
            Display::Off => return,
            Display::Value => value,
            Display::Name => a.name.clone(),
            Display::Both => format!("{}={}", a.name, value),
        };
        let placed = self.placed(at, &text, a.size, Xform::place(Point::default(), a.rot), a.rot.spin, a.align, 50.0);
        self.text(placed, a.layer, Some(part), None);
    }

    fn flatten(&mut self, prim: &Prim, cx: &Ctx, subst: &dyn Fn(&str) -> Option<String>) {
        let x = cx.xform;
        let layer = |l: u16| if cx.flip { flip_layer(l) } else { l };
        match prim {
            Prim::Wire {
                a,
                b,
                width,
                layer: l,
                curve,
                flat,
            } => {
                let curve = if x.mirrored() { -curve } else { *curve };
                let pts = arc_points(x.apply(*a), x.apply(*b), curve);
                self.push(
                    Shape::Line {
                        pts,
                        width: *width,
                        flat: *flat,
                    },
                    layer(*l),
                    cx.part,
                    cx.net,
                );
            }
            Prim::Circle { c, r, width, layer: l } => {
                self.push(
                    Shape::Circle {
                        c: x.apply(*c),
                        r: *r,
                        width: *width,
                    },
                    layer(*l),
                    cx.part,
                    cx.net,
                );
            }
            Prim::Rect { a, b, layer: l, rot } => {
                let local = x.then(Xform::place((*a + *b) * 0.5, Rot { angle: rot.angle, ..Rot::default() }));
                let (hw, hh) = ((b.x - a.x).abs() * 0.5, (b.y - a.y).abs() * 0.5);
                let pts = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
                    .map(|(px, py)| local.apply(Point::new(px, py)))
                    .to_vec();
                self.push(
                    Shape::Poly {
                        pts,
                        width: 0.0,
                        pour: false,
                    },
                    layer(*l),
                    cx.part,
                    cx.net,
                );
            }
            Prim::Polygon { verts, width, layer: l } => {
                let mut pts = Vec::new();
                for (i, (p, curve)) in verts.iter().enumerate() {
                    let next = verts[(i + 1) % verts.len()].0;
                    let curve = if x.mirrored() { -curve } else { *curve };
                    let arc = arc_points(x.apply(*p), x.apply(next), curve);
                    pts.extend_from_slice(&arc[..arc.len() - 1]);
                }
                if pts.len() >= 3 {
                    self.push(
                        Shape::Poly {
                            pts,
                            width: *width,
                            pour: cx.net.is_some(),
                        },
                        layer(*l),
                        cx.part,
                        cx.net,
                    );
                }
            }
            Prim::Text {
                at,
                text,
                size,
                layer: l,
                rot,
                align,
                distance,
            } => {
                let Some(text) = subst(text) else { return };
                let xform = x.then(Xform::place(Point::default(), *rot));
                let placed = self.placed(x.apply(*at), &text, *size, xform, rot.spin, *align, *distance);
                self.text(placed, layer(*l), cx.part, cx.net);
            }
            Prim::Hole { c, drill } => {
                self.push(
                    Shape::Drill {
                        c: x.apply(*c),
                        r: drill * 0.5,
                    },
                    45,
                    cx.part,
                    None,
                );
            }
            Prim::Smd {
                name,
                c,
                dx,
                dy,
                layer: l,
                rot,
                roundness,
            } => {
                let local = x.then(Xform::place(*c, *rot));
                let pts = rounded_rect(*dx, *dy, *roundness).into_iter().map(|p| local.apply(p)).collect();
                let net = cx.pad_nets.and_then(|m| m.get(name)).copied();
                let l = layer(*l);
                self.push(
                    Shape::Poly {
                        pts,
                        width: 0.0,
                        pour: false,
                    },
                    l,
                    cx.part,
                    net,
                );
                if let Some(part) = cx.part {
                    self.pads.push(PadInfo {
                        part,
                        pad: name.clone(),
                        at: x.apply(*c),
                        smd: true,
                        bottom: l == 16,
                        net,
                    });
                }
            }
            Prim::Pad {
                name,
                c,
                drill,
                diameter,
                shape,
                rot,
            } => {
                let d = self.restring.pad_diameter(*drill, *diameter);
                let local = x.then(Xform::place(*c, *rot));
                let net = cx.pad_nets.and_then(|m| m.get(name)).copied();
                let shape = pad_shape(*shape, d, local);
                self.push(shape, 17, cx.part, net);
                self.push(
                    Shape::Drill {
                        c: x.apply(*c),
                        r: drill * 0.5,
                    },
                    44,
                    cx.part,
                    net,
                );
                if let Some(part) = cx.part {
                    self.pads.push(PadInfo {
                        part,
                        pad: name.clone(),
                        at: x.apply(*c),
                        smd: false,
                        bottom: false,
                        net,
                    });
                }
            }
            Prim::Via { c, drill, diameter, shape } => {
                let d = self.restring.via_diameter(*drill, *diameter);
                let shape = pad_shape(*shape, d, x.then(Xform::translate(c.x, c.y)));
                self.push(shape, 18, cx.part, cx.net);
                self.push(
                    Shape::Drill {
                        c: x.apply(*c),
                        r: drill * 0.5,
                    },
                    44,
                    cx.part,
                    cx.net,
                );
            }
            Prim::Pin {
                name,
                c,
                length,
                rot,
                show_name,
                show_pad,
                function,
            } => self.pin(cx, name, *c, *length, *rot, *show_name, *show_pad, *function),
            Prim::Frame { a, b, layer: l, .. } => {
                let pts = [Point::new(a.x, a.y), Point::new(b.x, a.y), Point::new(b.x, b.y), Point::new(a.x, b.y), Point::new(a.x, a.y)]
                    .map(|p| x.apply(p))
                    .to_vec();
                self.push(
                    Shape::Line {
                        pts,
                        width: 0.254,
                        flat: false,
                    },
                    *l,
                    cx.part,
                    None,
                );
            }
            Prim::Junction { c } => {
                self.push(
                    Shape::Circle {
                        c: x.apply(*c),
                        r: 0.5,
                        width: 0.0,
                    },
                    91,
                    cx.part,
                    cx.net,
                );
            }
            Prim::Label {
                at,
                size,
                layer: l,
                rot,
                xref,
            } => {
                let Some(name) = cx.net_name else { return };
                let xform = x.then(Xform::place(Point::default(), *rot));
                let placed = self.placed(x.apply(*at), name, *size, xform, rot.spin, Align::default(), 50.0);
                if *xref {
                    let [c0, c1, c2, c3] = placed.corners();
                    let unit = |v: Point| if v.len() > 0.0 { v * (1.0 / v.len()) } else { v };
                    let (u, v) = (unit(c1 - c0), unit(c3 - c0));
                    let m = size * 0.35;
                    let pts = vec![c0 - u * m - v * m, c1 + u * m - v * m, c2 + u * m + v * m, c3 - u * m + v * m];
                    self.push(
                        Shape::Poly {
                            pts,
                            width: PIN_WIDTH,
                            pour: true,
                        },
                        *l,
                        cx.part,
                        cx.net,
                    );
                }
                self.text(placed, *l, cx.part, cx.net);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pin(
        &mut self,
        cx: &Ctx,
        name: &str,
        c: Point,
        length: f64,
        rot: Rot,
        show_name: bool,
        show_pad: bool,
        function: PinFunction,
    ) {
        let full = cx.xform.then(Xform::place(c, rot));
        let start = full.apply(Point::default());
        let net = cx.pins.and_then(|p| p.nets.get(name)).copied();
        let pads = cx.pins.and_then(|p| p.pads.get(name)).cloned().unwrap_or_default();
        let dot = matches!(function, PinFunction::Dot | PinFunction::DotClock);
        let line_end = if dot { (length - 1.27).max(0.0) } else { length };
        if line_end > 0.0 {
            self.push(
                Shape::Line {
                    pts: vec![start, full.apply(Point::new(line_end, 0.0))],
                    width: PIN_WIDTH,
                    flat: false,
                },
                94,
                cx.part,
                net,
            );
        }
        if dot {
            self.push(
                Shape::Circle {
                    c: full.apply(Point::new(length - 0.635, 0.0)),
                    r: 0.635,
                    width: PIN_WIDTH * 2.0,
                },
                94,
                cx.part,
                net,
            );
        }
        if matches!(function, PinFunction::Clock | PinFunction::DotClock) {
            let pts = [(length, 0.8), (length + 1.6, 0.0), (length, -0.8)]
                .map(|(px, py)| full.apply(Point::new(px, py)))
                .to_vec();
            self.push(
                Shape::Line {
                    pts,
                    width: PIN_WIDTH,
                    flat: false,
                },
                94,
                cx.part,
                net,
            );
        }
        let display = name.split('@').next().unwrap_or(name);
        if show_name && length > 0.0 {
            let placed = self.placed(
                full.apply(Point::new(length + 1.0, 0.0)),
                display,
                1.524,
                full,
                false,
                Align { h: -1, v: 0 },
                50.0,
            );
            self.text(placed, 95, cx.part, net);
        }
        if show_pad && !pads.is_empty() && length > 0.0 {
            // above horizontal pins, left of vertical ones, reading upright
            let angle = full.angle().rem_euclid(180.0);
            let upright = Xform::place(Point::default(), Rot { angle, ..Rot::default() });
            let normal = upright.apply(Point::new(0.0, 0.3));
            let mid = full.apply(Point::new(length * 0.5, 0.0)) + normal;
            let placed = Placed::new(mid, pads.join(" "), 1.27, upright, false, Align { h: 0, v: -1 }, 50.0);
            self.text(placed, 95, cx.part, net);
        }
        if let Some(part) = cx.part {
            self.pins.push(PinInfo {
                part,
                gate: cx.gate.to_string(),
                pin: name.to_string(),
                pads,
                sheet: cx.sheet,
                at: start,
                net,
            });
        }
    }

    fn finish(mut self, layers: &[eagle::Layer]) -> Scene {
        let kind = self.kind;
        self.items.sort_by_key(|i| rank(kind, i.layer));
        let mut bounds = Rect::EMPTY;
        let mut part_bounds: HashMap<u32, Rect> = HashMap::new();
        for item in self.items.iter().filter(|i| shown(kind, i.layer)) {
            bounds.union(item.bounds);
            if let (Some(part), false) = (item.part, matches!(item.shape, Shape::Text(_))) {
                part_bounds.entry(part).or_default().union(item.bounds);
            }
        }
        // a board's outline frames it better than everything around it
        if kind == Kind::Board {
            let mut outline = Rect::EMPTY;
            for item in self.items.iter().filter(|i| i.layer == 20) {
                outline.union(item.bounds);
            }
            if !outline.is_empty() {
                bounds = outline;
            }
        }
        Scene {
            kind,
            items: self.items,
            bounds,
            part_bounds,
            layer_colors: layers.iter().map(|l| (l.number, l.color)).collect(),
        }
    }
}

fn bounds_of(pts: &[Point]) -> Rect {
    let mut r = Rect::EMPTY;
    for p in pts {
        r.include(*p);
    }
    r
}

/// a rectangle `dx` by `dy` around the origin with rounded corners
fn rounded_rect(dx: f64, dy: f64, roundness: f64) -> Vec<Point> {
    let (hw, hh) = (dx * 0.5, dy * 0.5);
    let r = (roundness.clamp(0.0, 100.0) / 100.0) * hw.min(hh);
    if r <= 0.0 {
        return vec![Point::new(-hw, -hh), Point::new(hw, -hh), Point::new(hw, hh), Point::new(-hw, hh)];
    }
    let mut pts = Vec::new();
    for (cx, cy, start) in [(hw - r, hh - r, 0.0), (-hw + r, hh - r, 90.0), (-hw + r, -hh + r, 180.0), (hw - r, -hh + r, 270.0)] {
        for i in 0..=6 {
            let a = (start + 15.0 * i as f64).to_radians();
            pts.push(Point::new(cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts
}

/// a through-hole pad's copper of diameter `d`, placed by `xform`
fn pad_shape(shape: PadShape, d: f64, xform: Xform) -> Shape {
    let r = d * 0.5;
    match shape {
        PadShape::Round => Shape::Circle {
            c: xform.apply(Point::default()),
            r,
            width: 0.0,
        },
        PadShape::Square => Shape::Poly {
            pts: [(-r, -r), (r, -r), (r, r), (-r, r)].map(|(x, y)| xform.apply(Point::new(x, y))).to_vec(),
            width: 0.0,
            pour: false,
        },
        PadShape::Octagon => {
            let outer = r / (22.5f64).to_radians().cos();
            Shape::Poly {
                pts: (0..8)
                    .map(|i| {
                        let a = (22.5 + 45.0 * i as f64).to_radians();
                        xform.apply(Point::new(outer * a.cos(), outer * a.sin()))
                    })
                    .collect(),
                width: 0.0,
                pour: false,
            }
        }
        // elongated pads: a thick line with round ends, twice as long as wide
        PadShape::Long => Shape::Line {
            pts: vec![xform.apply(Point::new(-r, 0.0)), xform.apply(Point::new(r, 0.0))],
            width: d,
            flat: false,
        },
        PadShape::Offset => Shape::Line {
            pts: vec![xform.apply(Point::default()), xform.apply(Point::new(d, 0.0))],
            width: d,
            flat: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["R10", "R2", "C1", "R1", "U1A"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["C1", "R1", "R2", "R10", "U1A"]);
    }

    #[test]
    fn flip_layers() {
        assert_eq!(flip_layer(1), 16);
        assert_eq!(flip_layer(21), 22);
        assert_eq!(flip_layer(26), 25);
        assert_eq!(flip_layer(51), 52);
        assert_eq!(flip_layer(20), 20);
    }

    #[test]
    fn point_inside() {
        let square = [Point::new(0.0, 0.0), Point::new(1.0, 0.0), Point::new(1.0, 1.0), Point::new(0.0, 1.0)];
        assert!(inside(&square, Point::new(0.5, 0.5)));
        assert!(!inside(&square, Point::new(1.5, 0.5)));
    }
}
