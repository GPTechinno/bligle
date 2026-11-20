//! Eagle XML files (.sch / .brd, Eagle 6 and later) as plain data
//!
//! both files embed the libraries they use, so nothing else needs to be read

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};

use crate::geom::{Point, Rot};

#[derive(Clone, Debug)]
pub struct Layer {
    pub number: u16,
    pub name: String,
    pub color: u8,
}

/// text anchor: -1 left / bottom, 0 center, 1 right / top
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Align {
    pub h: i8,
    pub v: i8,
}

impl Default for Align {
    fn default() -> Self {
        Align { h: -1, v: -1 }
    }
}

impl Align {
    pub const CENTER: Align = Align { h: 0, v: 0 };

    fn parse(s: Option<&str>) -> Align {
        let Some(s) = s else { return Align::default() };
        if s == "center" {
            return Align::CENTER;
        }
        let (v, h) = s.split_once('-').unwrap_or((s, "center"));
        let side = |s: &str| match s {
            "left" | "bottom" => -1,
            "right" | "top" => 1,
            _ => 0,
        };
        Align { h: side(h), v: side(v) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadShape {
    Round,
    Square,
    Octagon,
    Long,
    Offset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinFunction {
    None,
    Dot,
    Clock,
    DotClock,
}

/// drawing primitives, in the coordinates of whatever holds them (symbol, package,
/// sheet, board)
#[derive(Clone, Debug)]
pub enum Prim {
    Wire {
        a: Point,
        b: Point,
        width: f64,
        layer: u16,
        /// arc angle in degrees, counter-clockwise from a to b when positive
        curve: f64,
        flat: bool,
    },
    Circle {
        c: Point,
        r: f64,
        /// 0 for a filled disc
        width: f64,
        layer: u16,
    },
    Rect {
        a: Point,
        b: Point,
        layer: u16,
        rot: Rot,
    },
    Polygon {
        /// vertices with the arc angle to the next one
        verts: Vec<(Point, f64)>,
        width: f64,
        layer: u16,
    },
    Text {
        at: Point,
        text: String,
        size: f64,
        layer: u16,
        rot: Rot,
        align: Align,
        /// line spacing in percent of the size
        distance: f64,
    },
    Hole {
        c: Point,
        drill: f64,
    },
    Smd {
        name: String,
        c: Point,
        dx: f64,
        dy: f64,
        layer: u16,
        rot: Rot,
        /// percent of the half smaller side
        roundness: f64,
    },
    Pad {
        name: String,
        c: Point,
        drill: f64,
        /// 0: from the design rules
        diameter: f64,
        shape: PadShape,
        rot: Rot,
    },
    Via {
        c: Point,
        drill: f64,
        diameter: f64,
        shape: PadShape,
    },
    Pin {
        name: String,
        c: Point,
        length: f64,
        rot: Rot,
        show_name: bool,
        show_pad: bool,
        function: PinFunction,
    },
    Frame {
        a: Point,
        b: Point,
        layer: u16,
    },
    Junction {
        c: Point,
    },
    Label {
        at: Point,
        size: f64,
        layer: u16,
        rot: Rot,
        xref: bool,
    },
}

#[derive(Default, Debug)]
pub struct Library {
    pub symbols: HashMap<String, Vec<Prim>>,
    pub packages: HashMap<String, Package>,
    pub devicesets: HashMap<String, DeviceSet>,
}

#[derive(Default, Debug)]
pub struct Package {
    pub description: String,
    pub prims: Vec<Prim>,
}

#[derive(Default, Debug)]
pub struct DeviceSet {
    pub description: String,
    pub gates: Vec<Gate>,
    pub devices: HashMap<String, Device>,
}

#[derive(Debug)]
pub struct Gate {
    pub name: String,
    pub symbol: String,
}

#[derive(Default, Debug)]
pub struct Device {
    pub package: Option<String>,
    pub connects: Vec<Connect>,
    pub technologies: HashMap<String, Vec<(String, String)>>,
}

#[derive(Debug)]
pub struct Connect {
    pub gate: String,
    pub pin: String,
    pub pads: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Off,
    Value,
    Name,
    Both,
}

/// an attribute of a part, placed on the drawing when the part is smashed
#[derive(Clone, Debug)]
pub struct Attr {
    pub name: String,
    pub value: Option<String>,
    /// absolute position, when placed
    pub at: Option<Point>,
    pub size: f64,
    pub layer: u16,
    pub rot: Rot,
    pub align: Align,
    pub display: Display,
}

#[derive(Debug)]
pub struct Part {
    pub name: String,
    pub library: String,
    pub deviceset: String,
    pub device: String,
    pub technology: String,
    pub value: Option<String>,
    pub attributes: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct Instance {
    pub part: String,
    pub gate: String,
    pub at: Point,
    pub rot: Rot,
    pub smashed: bool,
    pub attrs: Vec<Attr>,
}

#[derive(Debug)]
pub struct PinRef {
    pub part: String,
    pub gate: String,
    pub pin: String,
}

#[derive(Debug, Default)]
pub struct Segment {
    /// wires, junctions and labels
    pub prims: Vec<Prim>,
    pub pinrefs: Vec<PinRef>,
}

#[derive(Debug)]
pub struct SchNet {
    pub name: String,
    pub class: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Default)]
pub struct Sheet {
    pub plain: Vec<Prim>,
    pub instances: Vec<Instance>,
    pub nets: Vec<SchNet>,
    pub busses: Vec<SchNet>,
}

#[derive(Debug)]
pub struct Schematic {
    pub version: String,
    pub layers: Vec<Layer>,
    pub libraries: HashMap<String, Library>,
    pub attributes: Vec<(String, String)>,
    pub parts: Vec<Part>,
    pub sheets: Vec<Sheet>,
}

#[derive(Debug)]
pub struct Element {
    pub name: String,
    pub library: String,
    pub package: String,
    pub value: String,
    pub at: Point,
    pub rot: Rot,
    pub smashed: bool,
    pub attrs: Vec<Attr>,
}

#[derive(Debug)]
pub struct Signal {
    pub name: String,
    pub class: String,
    /// (element, pad)
    pub contacts: Vec<(String, String)>,
    /// wires, vias and polygons
    pub prims: Vec<Prim>,
}

/// pad and via annular rings from the board's design rules, in mm
#[derive(Clone, Copy, Debug)]
pub struct Restring {
    pub pad_fraction: f64,
    pub pad_min: f64,
    pub pad_max: f64,
    pub via_fraction: f64,
    pub via_min: f64,
    pub via_max: f64,
}

impl Default for Restring {
    fn default() -> Self {
        Restring {
            pad_fraction: 0.25,
            pad_min: 0.254,
            pad_max: 0.508,
            via_fraction: 0.25,
            via_min: 0.2032,
            via_max: 0.508,
        }
    }
}

impl Restring {
    pub fn pad_diameter(&self, drill: f64, diameter: f64) -> f64 {
        diameter.max(drill + 2.0 * (drill * self.pad_fraction).clamp(self.pad_min, self.pad_max))
    }

    pub fn via_diameter(&self, drill: f64, diameter: f64) -> f64 {
        diameter.max(drill + 2.0 * (drill * self.via_fraction).clamp(self.via_min, self.via_max))
    }
}

#[derive(Debug)]
pub struct Board {
    pub version: String,
    pub layers: Vec<Layer>,
    pub libraries: HashMap<String, Library>,
    pub plain: Vec<Prim>,
    pub elements: Vec<Element>,
    pub signals: Vec<Signal>,
    pub restring: Restring,
}

// ------------------------------------------------------------------ parsing

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Eagle files start with `<!DOCTYPE eagle SYSTEM "eagle.dtd">`
fn parse_xml(text: &str) -> Result<Document<'_>, roxmltree::Error> {
    Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
}

fn drawing<'a>(doc: &'a Document) -> Result<(String, Node<'a, 'a>)> {
    let eagle = doc.root_element();
    if eagle.tag_name().name() != "eagle" {
        bail!("not an Eagle XML file (Eagle 6 or later)");
    }
    let version = eagle.attribute("version").unwrap_or("").to_string();
    let drawing = child(eagle, "drawing").context("no <drawing>")?;
    Ok((version, drawing))
}

pub fn parse_schematic(path: &Path) -> Result<Schematic> {
    let text = read(path)?;
    let doc = parse_xml(&text).with_context(|| format!("parsing {}", path.display()))?;
    let (version, drawing) = drawing(&doc)?;
    let schematic = child(drawing, "schematic").context("this file has no <schematic> (a board?)")?;
    let parts = children(child(schematic, "parts"), "part")
        .map(|n| Part {
            name: s(n, "name"),
            library: s(n, "library"),
            deviceset: s(n, "deviceset"),
            device: s(n, "device"),
            technology: s(n, "technology"),
            value: n.attribute("value").map(str::to_string),
            attributes: children(Some(n), "attribute")
                .map(|a| (s(a, "name"), s(a, "value")))
                .collect(),
        })
        .collect();
    let sheets = children(child(schematic, "sheets"), "sheet")
        .map(|sheet| Sheet {
            plain: prims(child(sheet, "plain")),
            instances: children(child(sheet, "instances"), "instance")
                .map(|n| Instance {
                    part: s(n, "part"),
                    gate: s(n, "gate"),
                    at: point(n, "x", "y"),
                    rot: rot(n),
                    smashed: n.attribute("smashed") == Some("yes"),
                    attrs: children(Some(n), "attribute").map(attr).collect(),
                })
                .collect(),
            nets: nets(child(sheet, "nets"), "net"),
            busses: nets(child(sheet, "busses"), "bus"),
        })
        .collect();
    Ok(Schematic {
        version,
        layers: layers(drawing),
        libraries: libraries(child(schematic, "libraries")),
        attributes: children(child(schematic, "attributes"), "attribute")
            .map(|a| (s(a, "name"), s(a, "value")))
            .collect(),
        parts,
        sheets,
    })
}

pub fn parse_board(path: &Path) -> Result<Board> {
    let text = read(path)?;
    let doc = parse_xml(&text).with_context(|| format!("parsing {}", path.display()))?;
    let (version, drawing) = drawing(&doc)?;
    let board = child(drawing, "board").context("this file has no <board> (a schematic?)")?;
    let elements = children(child(board, "elements"), "element")
        .map(|n| Element {
            name: s(n, "name"),
            library: s(n, "library"),
            package: s(n, "package"),
            value: s(n, "value"),
            at: point(n, "x", "y"),
            rot: rot(n),
            smashed: n.attribute("smashed") == Some("yes"),
            attrs: children(Some(n), "attribute").map(attr).collect(),
        })
        .collect();
    let signals = children(child(board, "signals"), "signal")
        .map(|n| Signal {
            name: s(n, "name"),
            class: s(n, "class"),
            contacts: children(Some(n), "contactref")
                .map(|c| (s(c, "element"), s(c, "pad")))
                .collect(),
            prims: prims(Some(n)),
        })
        .collect();
    Ok(Board {
        version,
        layers: layers(drawing),
        libraries: libraries(child(board, "libraries")),
        plain: prims(child(board, "plain")),
        elements,
        signals,
        restring: restring(child(board, "designrules")),
    })
}

fn child<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children().find(|n| n.has_tag_name(name))
}

fn children<'a>(node: Option<Node<'a, 'a>>, name: &'a str) -> impl Iterator<Item = Node<'a, 'a>> + 'a {
    node.into_iter()
        .flat_map(move |n| n.children().filter(move |c| c.has_tag_name(name)))
}

fn s(node: Node, name: &str) -> String {
    node.attribute(name).unwrap_or("").to_string()
}

fn f(node: Node, name: &str) -> f64 {
    node.attribute(name).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

fn point(node: Node, x: &str, y: &str) -> Point {
    Point::new(f(node, x), f(node, y))
}

fn rot(node: Node) -> Rot {
    node.attribute("rot").map(Rot::parse).unwrap_or_default()
}

fn layer(node: Node) -> u16 {
    node.attribute("layer").and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn pad_shape(node: Node) -> PadShape {
    match node.attribute("shape") {
        Some("square") => PadShape::Square,
        Some("octagon") => PadShape::Octagon,
        Some("long") => PadShape::Long,
        Some("offset") => PadShape::Offset,
        _ => PadShape::Round,
    }
}

fn layers(drawing: Node) -> Vec<Layer> {
    children(child(drawing, "layers"), "layer")
        .map(|n| Layer {
            number: n.attribute("number").and_then(|v| v.parse().ok()).unwrap_or(0),
            name: s(n, "name"),
            color: n.attribute("color").and_then(|v| v.parse().ok()).unwrap_or(7),
        })
        .collect()
}

fn attr(n: Node) -> Attr {
    Attr {
        name: s(n, "name"),
        value: n.attribute("value").map(str::to_string),
        at: n.attribute("x").map(|_| point(n, "x", "y")),
        size: f(n, "size"),
        layer: layer(n),
        rot: rot(n),
        align: Align::parse(n.attribute("align")),
        display: match n.attribute("display") {
            Some("off") => Display::Off,
            Some("name") => Display::Name,
            Some("both") => Display::Both,
            _ => Display::Value,
        },
    }
}

fn nets(node: Option<Node>, tag: &'static str) -> Vec<SchNet> {
    children(node, tag)
        .map(|n| SchNet {
            name: s(n, "name"),
            class: s(n, "class"),
            segments: children(Some(n), "segment")
                .map(|seg| Segment {
                    prims: prims(Some(seg)),
                    pinrefs: children(Some(seg), "pinref")
                        .map(|p| PinRef {
                            part: s(p, "part"),
                            gate: s(p, "gate"),
                            pin: s(p, "pin"),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

fn libraries(node: Option<Node>) -> HashMap<String, Library> {
    children(node, "library")
        .map(|lib| {
            let symbols = children(child(lib, "symbols"), "symbol")
                .map(|n| (s(n, "name"), prims(Some(n))))
                .collect();
            let packages = children(child(lib, "packages"), "package")
                .map(|n| {
                    (
                        s(n, "name"),
                        Package {
                            description: description(n),
                            prims: prims(Some(n)),
                        },
                    )
                })
                .collect();
            let devicesets = children(child(lib, "devicesets"), "deviceset")
                .map(|n| {
                    let gates = children(child(n, "gates"), "gate")
                        .map(|g| Gate {
                            name: s(g, "name"),
                            symbol: s(g, "symbol"),
                        })
                        .collect();
                    let devices = children(child(n, "devices"), "device")
                        .map(|d| {
                            let connects = children(child(d, "connects"), "connect")
                                .map(|c| Connect {
                                    gate: s(c, "gate"),
                                    pin: s(c, "pin"),
                                    pads: s(c, "pad").split_whitespace().map(str::to_string).collect(),
                                })
                                .collect();
                            let technologies = children(child(d, "technologies"), "technology")
                                .map(|t| {
                                    let attrs = children(Some(t), "attribute")
                                        .map(|a| (s(a, "name"), s(a, "value")))
                                        .collect();
                                    (s(t, "name"), attrs)
                                })
                                .collect();
                            (
                                s(d, "name"),
                                Device {
                                    package: d.attribute("package").map(str::to_string),
                                    connects,
                                    technologies,
                                },
                            )
                        })
                        .collect();
                    (
                        s(n, "name"),
                        DeviceSet {
                            description: description(n),
                            gates,
                            devices,
                        },
                    )
                })
                .collect();
            (
                s(lib, "name"),
                Library {
                    symbols,
                    packages,
                    devicesets,
                },
            )
        })
        .collect()
}

/// first line of a description, without its HTML tags
fn description(node: Node) -> String {
    let Some(text) = child(node, "description").and_then(|d| d.text()) else {
        return String::new();
    };
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// a value of the design rules like `10mil`, `0.2mm` or `0.25`, in mm
fn rule_value(v: &str) -> Option<f64> {
    let v = v.trim();
    let (number, unit) = match v.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&v[..i], &v[i..]),
        None => (v, ""),
    };
    let number: f64 = number.parse().ok()?;
    Some(match unit {
        "mil" => number * 0.0254,
        "inch" => number * 25.4,
        "mic" => number / 1000.0,
        _ => number,
    })
}

fn restring(node: Option<Node>) -> Restring {
    let mut r = Restring::default();
    for p in children(node, "param") {
        // values may list one per layer; the first (top) is enough to draw
        let Some(value) = p
            .attribute("value")
            .and_then(|v| v.split_whitespace().next())
            .and_then(rule_value)
        else {
            continue;
        };
        match p.attribute("name").unwrap_or("") {
            "rvPadTop" => r.pad_fraction = value,
            "rlMinPadTop" => r.pad_min = value,
            "rlMaxPadTop" => r.pad_max = value,
            "rvViaOuter" => r.via_fraction = value,
            "rlMinViaOuter" => r.via_min = value,
            "rlMaxViaOuter" => r.via_max = value,
            _ => {}
        }
    }
    r
}

/// the drawing primitives directly under `node`
fn prims(node: Option<Node>) -> Vec<Prim> {
    let Some(node) = node else { return Vec::new() };
    node.children().filter(|n| n.is_element()).filter_map(prim).collect()
}

fn prim(n: Node) -> Option<Prim> {
    Some(match n.tag_name().name() {
        "wire" => Prim::Wire {
            a: point(n, "x1", "y1"),
            b: point(n, "x2", "y2"),
            width: f(n, "width"),
            layer: layer(n),
            curve: f(n, "curve"),
            flat: n.attribute("cap") == Some("flat"),
        },
        "circle" => Prim::Circle {
            c: point(n, "x", "y"),
            r: f(n, "radius"),
            width: f(n, "width"),
            layer: layer(n),
        },
        "rectangle" => Prim::Rect {
            a: point(n, "x1", "y1"),
            b: point(n, "x2", "y2"),
            layer: layer(n),
            rot: rot(n),
        },
        "polygon" => Prim::Polygon {
            verts: children(Some(n), "vertex")
                .map(|v| (point(v, "x", "y"), f(v, "curve")))
                .collect(),
            width: f(n, "width"),
            layer: layer(n),
        },
        "text" => Prim::Text {
            at: point(n, "x", "y"),
            text: n.text().unwrap_or("").to_string(),
            size: f(n, "size"),
            layer: layer(n),
            rot: rot(n),
            align: Align::parse(n.attribute("align")),
            distance: n.attribute("distance").and_then(|v| v.parse().ok()).unwrap_or(50.0),
        },
        "hole" => Prim::Hole {
            c: point(n, "x", "y"),
            drill: f(n, "drill"),
        },
        "smd" => Prim::Smd {
            name: s(n, "name"),
            c: point(n, "x", "y"),
            dx: f(n, "dx"),
            dy: f(n, "dy"),
            layer: layer(n),
            rot: rot(n),
            roundness: f(n, "roundness"),
        },
        "pad" => Prim::Pad {
            name: s(n, "name"),
            c: point(n, "x", "y"),
            drill: f(n, "drill"),
            diameter: f(n, "diameter"),
            shape: pad_shape(n),
            rot: rot(n),
        },
        "via" => Prim::Via {
            c: point(n, "x", "y"),
            drill: f(n, "drill"),
            diameter: f(n, "diameter"),
            shape: pad_shape(n),
        },
        "pin" => {
            let visible = n.attribute("visible").unwrap_or("both");
            Prim::Pin {
                name: s(n, "name"),
                c: point(n, "x", "y"),
                length: match n.attribute("length") {
                    Some("point") => 0.0,
                    Some("short") => 2.54,
                    Some("middle") => 5.08,
                    _ => 7.62,
                },
                rot: rot(n),
                show_name: matches!(visible, "pin" | "both"),
                show_pad: matches!(visible, "pad" | "both"),
                function: match n.attribute("function") {
                    Some("dot") => PinFunction::Dot,
                    Some("clk") => PinFunction::Clock,
                    Some("dotclk") => PinFunction::DotClock,
                    _ => PinFunction::None,
                },
            }
        }
        "frame" => Prim::Frame {
            a: point(n, "x1", "y1"),
            b: point(n, "x2", "y2"),
            layer: layer(n),
        },
        "junction" => Prim::Junction { c: point(n, "x", "y") },
        "label" => Prim::Label {
            at: point(n, "x", "y"),
            size: f(n, "size"),
            layer: layer(n),
            rot: rot(n),
            xref: n.attribute("xref") == Some("yes"),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align() {
        assert_eq!(Align::parse(None), Align { h: -1, v: -1 });
        assert_eq!(Align::parse(Some("top-right")), Align { h: 1, v: 1 });
        assert_eq!(Align::parse(Some("center-left")), Align { h: -1, v: 0 });
        assert_eq!(Align::parse(Some("center")), Align::CENTER);
    }

    #[test]
    fn rules() {
        assert!((rule_value("10mil").unwrap() - 0.254).abs() < 1e-9);
        assert_eq!(rule_value("0.25"), Some(0.25));
        assert_eq!(rule_value("0.2mm"), Some(0.2));
    }
}
