//! headless mode: queries about a design for people and AI agents (text or JSON), PNG
//! renders of the drawings, and screenshots of the whole UI rendered offscreen with
//! Blit's CPU renderer (optionally after scripted input)

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::BufRead,
    path::Path,
    rc::Rc,
    sync::{Arc, atomic::AtomicU32},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use blit::{Frame, FrameInfo, Input, Key, KeyInput, LogicalPoint, Modifiers, MouseButton, ScrollPhase, Size};
use blit_cpu::{Renderer, RendererConfig, VecBuffer, Xrgb8888};
use blit_desktop::{DesktopContext, GraphicsBackend, GraphicsError, Ui};
use blit_gui::{
    GuiContext, RenderInput, TextSystem,
    image::{ImageData, ImageHandle},
};
use serde_json::{Map, Value, json};

use crate::{
    app::App,
    design::{Design, Kind, PadInfo, Selection, natural_cmp},
    eagle::Prim,
    geom::{Rect, Viewport, arc_points},
    render,
};

pub const COMMANDS: &str = "\
commands (headless, for people and AI agents; add --json for JSON):
  info                        what the design holds
  parts [FILTER]              parts: value, package, sheet, board position
  part NAME                   one part: attributes, pins and pads with their nets
  nets [FILTER]               nets: pins, pads, sheets, routing
  net NAME                    one net: pins, pads, tracks, vias
  find TEXT                   parts and nets matching a text (names, values, attributes)
  check                       schematic / board consistency (missing parts, values,
                              packages, pin vs pad nets, unrouted connections)
  render OUT.png [--view sch|brd|both] [--select NAME] [--sheet N] [--size WxH]
                              [--actions 'click:X,Y;dclick:X,Y;drag:X0,Y0,X1,Y1;wheel:X,Y,N;key:K;text:S']
                              draw the schematic or the board (zoomed on the selection),
                              or the whole UI (both, the default) after scripted input
  shell                       read commands from stdin, one per line, the design loaded once
";

/// runs one headless command; `args` holds the command and its arguments
pub fn run(design: &Arc<Design>, args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let args: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    let Some((&command, rest)) = args.split_first() else {
        bail!("no command\n\n{COMMANDS}")
    };
    let value = match command {
        "info" => info(design),
        "parts" => parts(design, rest.first().copied()),
        "part" => part(design, rest.first().context("part NAME")?)?,
        "nets" => nets(design, rest.first().copied()),
        "net" => net(design, rest.first().context("net NAME")?)?,
        "find" | "search" => find(design, &rest.join(" ")),
        "check" => check(design),
        "render" | "screenshot" => {
            return render_command(design, rest);
        }
        "shell" => return shell(design),
        "help" => {
            print!("{COMMANDS}");
            return Ok(());
        }
        other => bail!("unknown command {other:?}\n\n{COMMANDS}"),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let mut out = String::new();
        human(&mut out, &value, 0);
        print!("{out}");
    }
    Ok(())
}

fn shell(design: &Arc<Design>) -> Result<()> {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let words = split_words(&line);
        if words.is_empty() || words[0].starts_with('#') {
            continue;
        }
        if matches!(words[0].as_str(), "quit" | "exit") {
            break;
        }
        if let Err(error) = run(design, &words) {
            println!("error: {error:#}");
        }
        println!();
    }
    Ok(())
}

/// splits a command line on spaces, keeping "quoted words" together
fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' | '\'' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

// ---------------------------------------------------------------- queries

fn round(v: f64) -> Value {
    json!((v * 1000.0).round() / 1000.0)
}

fn rel(path: Option<&Path>) -> Value {
    path.map_or(Value::Null, |p| json!(p.display().to_string()))
}

fn info(d: &Design) -> Value {
    let mut v = Map::new();
    v.insert("design".into(), json!(d.name));
    v.insert("schematic".into(), rel(d.sch_path.as_deref()));
    v.insert("board".into(), rel(d.brd_path.as_deref()));
    let version = d
        .sch
        .as_ref()
        .map(|s| &s.version)
        .or(d.brd.as_ref().map(|b| &b.version));
    v.insert("eagle_version".into(), json!(version));
    v.insert("sheets".into(), json!(d.sheets.len()));
    let in_sch = d.parts.iter().filter(|p| p.sch.is_some()).count();
    let on_brd = d.parts.iter().filter(|p| p.brd.is_some()).count();
    let packaged = (0..d.parts.len() as u32)
        .filter(|&p| d.sch_package(p).is_some())
        .count();
    v.insert(
        "parts".into(),
        json!({"schematic": in_sch, "with_package": packaged, "board": on_brd}),
    );
    let sch_nets = d.nets.iter().filter(|n| n.in_sch).count();
    let brd_nets = d.nets.iter().filter(|n| n.in_brd).count();
    v.insert("nets".into(), json!({"schematic": sch_nets, "board": brd_nets}));
    if let Some(board) = &d.board {
        v.insert(
            "board_size_mm".into(),
            json!([round(board.bounds.width()), round(board.bounds.height())]),
        );
        v.insert(
            "board_origin_mm".into(),
            json!([round(board.bounds.min.x), round(board.bounds.min.y)]),
        );
    }
    if let Some(brd) = &d.brd {
        let mut copper = BTreeSet::new();
        let mut vias = 0;
        for prim in brd.signals.iter().flat_map(|s| s.prims.iter()) {
            match prim {
                Prim::Wire { layer, .. } if (1..=16).contains(layer) => {
                    copper.insert(*layer);
                }
                Prim::Polygon { layer, .. } if (1..=16).contains(layer) => {
                    copper.insert(*layer);
                }
                Prim::Via { .. } => vias += 1,
                _ => {}
            }
        }
        let names: Vec<&str> = copper.iter().map(|l| layer_name(d, *l)).collect();
        v.insert("copper_layers".into(), json!(names));
        v.insert("vias".into(), json!(vias));
        let sides = brd
            .elements
            .iter()
            .fold((0, 0), |(t, b), e| if e.rot.mirror { (t, b + 1) } else { (t + 1, b) });
        v.insert("elements_top_bottom".into(), json!([sides.0, sides.1]));
    }
    let mut libraries: BTreeSet<&str> = BTreeSet::new();
    for lib in d
        .sch
        .iter()
        .flat_map(|s| s.libraries.keys())
        .chain(d.brd.iter().flat_map(|b| b.libraries.keys()))
    {
        libraries.insert(lib);
    }
    v.insert("libraries".into(), json!(libraries));
    if let Some(sch) = &d.sch
        && !sch.attributes.is_empty()
    {
        let attrs: Map<String, Value> = sch.attributes.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        v.insert("attributes".into(), Value::Object(attrs));
    }
    Value::Object(v)
}

fn layer_name(d: &Design, layer: u16) -> &str {
    d.brd
        .iter()
        .flat_map(|b| b.layers.iter())
        .chain(d.sch.iter().flat_map(|s| s.layers.iter()))
        .find(|l| l.number == layer)
        .map_or("?", |l| l.name.as_str())
}

fn matches(filter: Option<&str>, fields: &[&str]) -> bool {
    let Some(filter) = filter else { return true };
    let filter = filter.to_ascii_lowercase();
    fields.iter().any(|f| f.to_ascii_lowercase().contains(&filter))
}

fn board_place(d: &Design, part: u32) -> Option<Value> {
    let e = d.element(part)?;
    Some(json!({
        "x": round(e.at.x),
        "y": round(e.at.y),
        "rot": e.rot.to_eagle(),
        "side": if e.rot.mirror { "bottom" } else { "top" },
    }))
}

fn sheets_of(d: &Design, part: u32) -> Vec<usize> {
    let mut sheets: Vec<usize> = d.sch_locations(part).into_iter().map(|(s, _)| s + 1).collect();
    sheets.dedup();
    sheets
}

fn parts(d: &Design, filter: Option<&str>) -> Value {
    let mut list = Vec::new();
    for (i, p) in d.parts.iter().enumerate() {
        let id = i as u32;
        let value = d.value(id);
        let package = d
            .element(id)
            .map(|e| e.package.clone())
            .or(d.sch_package(id).map(str::to_string));
        if !matches(filter, &[&p.name, &value, package.as_deref().unwrap_or("")]) {
            continue;
        }
        let mut v = Map::new();
        v.insert("name".into(), json!(p.name));
        v.insert("value".into(), json!(value));
        v.insert("package".into(), json!(package));
        let sheets = sheets_of(d, id);
        if !sheets.is_empty() {
            v.insert("sheet".into(), json!(sheets));
        }
        if let Some(place) = board_place(d, id) {
            v.insert("board".into(), place);
        } else if p.sch.is_some() && d.sch_package(id).is_none() {
            v.insert("board".into(), json!("no package (symbol only)"));
        } else {
            v.insert("board".into(), json!("missing"));
        }
        list.push(Value::Object(v));
    }
    json!({ "count": list.len(), "parts": list })
}

fn find_part(d: &Design, name: &str) -> Result<u32> {
    match d.find(name) {
        Selection::Part(p) => Ok(p),
        _ => {
            let close: Vec<&str> = d
                .parts
                .iter()
                .filter(|p| {
                    p.name
                        .to_ascii_lowercase()
                        .starts_with(&name.to_ascii_lowercase()[..1.min(name.len())])
                })
                .map(|p| p.name.as_str())
                .take(12)
                .collect();
            bail!("no part named {name:?} (parts like it: {})", close.join(", "))
        }
    }
}

fn find_net(d: &Design, name: &str) -> Result<u32> {
    match d.find(name) {
        Selection::Net(n) => Ok(n),
        Selection::Part(_) => bail!("{name:?} is a part; try `part {name}`"),
        Selection::None => bail!("no net named {name:?}"),
    }
}

fn net_name(d: &Design, net: Option<u32>) -> Value {
    net.map_or(Value::Null, |n| json!(d.nets[n as usize].name))
}

fn part(d: &Design, name: &str) -> Result<Value> {
    let id = find_part(d, name)?;
    let p = &d.parts[id as usize];
    let mut v = Map::new();
    v.insert("name".into(), json!(p.name));
    v.insert("value".into(), json!(d.value(id)));
    let description = d.description(id);
    if !description.is_empty() {
        v.insert("description".into(), json!(description));
    }
    if let Some(sp) = d.sch_part(id) {
        v.insert("library".into(), json!(sp.library));
        v.insert("deviceset".into(), json!(sp.deviceset));
        v.insert("device".into(), json!(sp.device));
        if !sp.technology.is_empty() {
            v.insert("technology".into(), json!(sp.technology));
        }
    }
    let package = d.element(id).map(|e| e.package.as_str()).or(d.sch_package(id));
    v.insert("package".into(), json!(package));
    if let Some(e) = d.element(id)
        && let Some(pkg) = d
            .brd
            .as_ref()
            .and_then(|b| b.libraries.get(&e.library)?.packages.get(&e.package))
        && !pkg.description.is_empty()
    {
        v.insert("package_description".into(), json!(pkg.description));
    }
    let attrs: Map<String, Value> = d.attributes(id).into_iter().map(|(k, val)| (k, json!(val))).collect();
    if !attrs.is_empty() {
        v.insert("attributes".into(), Value::Object(attrs));
    }
    let gates: Vec<Value> = d
        .sch
        .iter()
        .flat_map(|s| s.sheets.iter().enumerate())
        .flat_map(|(i, sheet)| sheet.instances.iter().map(move |inst| (i, inst)))
        .filter(|(_, inst)| inst.part == p.name)
        .map(|(i, inst)| {
            json!({"gate": inst.gate, "sheet": i + 1, "x": round(inst.at.x), "y": round(inst.at.y), "rot": inst.rot.to_eagle()})
        })
        .collect();
    if !gates.is_empty() {
        v.insert("schematic".into(), json!(gates));
    }
    v.insert("board".into(), board_place(d, id).unwrap_or(Value::Null));

    // connections: the schematic's pins with their pads, and the board's pads
    // pad -> (schematic pins, schematic net, board net, board pad)
    type Row<'a> = (Vec<String>, Option<u32>, Option<u32>, Option<&'a PadInfo>);
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut pins_only = Vec::new();
    for pin in d.pins.iter().filter(|pin| pin.part == id) {
        let label = if pin.gate.is_empty() || d.sch_locations(id).len() <= 1 {
            pin.pin.clone()
        } else {
            format!("{}.{}", pin.gate, pin.pin)
        };
        if pin.pads.is_empty() {
            pins_only.push(json!({"pin": label, "net": net_name(d, pin.net)}));
        }
        for pad in &pin.pads {
            let row = rows.entry(pad.clone()).or_default();
            row.0.push(label.clone());
            row.1 = row.1.or(pin.net);
        }
    }
    for pad in d.pads.iter().filter(|pad| pad.part == id) {
        let row = rows.entry(pad.pad.clone()).or_default();
        row.2 = pad.net;
        row.3 = Some(pad);
    }
    let mut pads: Vec<(String, Value)> = rows
        .into_iter()
        .map(|(pad, (pins, sch_net, brd_net, info))| {
            let mut row = Map::new();
            row.insert("pad".into(), json!(pad));
            if !pins.is_empty() {
                row.insert("pin".into(), json!(pins.join(" ")));
            }
            row.insert("net".into(), net_name(d, sch_net.or(brd_net)));
            if d.sch_part(id).is_some() && d.element(id).is_some() && sch_net != brd_net {
                row.insert("board_net".into(), net_name(d, brd_net));
                row.insert("mismatch".into(), json!(true));
            }
            if let Some(info) = info {
                row.insert("x".into(), round(info.at.x));
                row.insert("y".into(), round(info.at.y));
                let kind = match (info.smd, info.bottom) {
                    (false, _) => "tht",
                    (true, false) => "smd top",
                    (true, true) => "smd bottom",
                };
                row.insert("type".into(), json!(kind));
            }
            (pad, Value::Object(row))
        })
        .collect();
    pads.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    if !pads.is_empty() {
        v.insert(
            "pads".into(),
            json!(pads.into_iter().map(|(_, v)| v).collect::<Vec<_>>()),
        );
    }
    if !pins_only.is_empty() {
        v.insert("pins_without_pad".into(), json!(pins_only));
    }
    let mut nets: BTreeSet<&str> = BTreeSet::new();
    for n in d.pins.iter().filter(|p| p.part == id).filter_map(|p| p.net) {
        nets.insert(&d.nets[n as usize].name);
    }
    for n in d.pads.iter().filter(|p| p.part == id).filter_map(|p| p.net) {
        nets.insert(&d.nets[n as usize].name);
    }
    v.insert("nets".into(), json!(nets));
    Ok(Value::Object(v))
}

/// routing of a board signal
struct Routing {
    length: BTreeMap<u16, f64>,
    vias: usize,
    polygons: Vec<u16>,
    airwires: usize,
}

fn routing(d: &Design, name: &str) -> Option<Routing> {
    let signal = d.brd.as_ref()?.signals.iter().find(|s| s.name == name)?;
    let mut r = Routing {
        length: BTreeMap::new(),
        vias: 0,
        polygons: Vec::new(),
        airwires: 0,
    };
    for prim in &signal.prims {
        match prim {
            Prim::Wire { a, b, layer: 19, .. } if a != b => r.airwires += 1,
            Prim::Wire { a, b, layer, curve, .. } if (1..=16).contains(layer) => {
                let pts = arc_points(*a, *b, *curve);
                let len: f64 = pts.windows(2).map(|w| w[0].dist(w[1])).sum();
                *r.length.entry(*layer).or_default() += len;
            }
            Prim::Via { .. } => r.vias += 1,
            Prim::Polygon { layer, .. } => r.polygons.push(*layer),
            _ => {}
        }
    }
    Some(r)
}

fn nets(d: &Design, filter: Option<&str>) -> Value {
    let mut list = Vec::new();
    for (i, n) in d.nets.iter().enumerate() {
        if !matches(filter, &[&n.name]) {
            continue;
        }
        let id = i as u32;
        let pins = d.pins.iter().filter(|p| p.net == Some(id)).count();
        let pads = d.pads.iter().filter(|p| p.net == Some(id)).count();
        let mut v = Map::new();
        v.insert("name".into(), json!(n.name));
        v.insert("pins".into(), json!(pins));
        v.insert("pads".into(), json!(pads));
        let sheets: Vec<usize> = d.net_sheets(id).into_iter().map(|s| s + 1).collect();
        if !sheets.is_empty() {
            v.insert("sheet".into(), json!(sheets));
        }
        if let Some(r) = routing(d, &n.name) {
            let total: f64 = r.length.values().sum();
            v.insert("track_mm".into(), round(total));
            if r.airwires > 0 {
                v.insert("unrouted".into(), json!(r.airwires));
            }
        } else if n.in_sch {
            v.insert("board".into(), json!("missing"));
        }
        list.push(Value::Object(v));
    }
    json!({ "count": list.len(), "nets": list })
}

fn net(d: &Design, name: &str) -> Result<Value> {
    let id = find_net(d, name)?;
    let n = &d.nets[id as usize];
    let mut v = Map::new();
    v.insert("name".into(), json!(n.name));
    let class = d
        .sch
        .iter()
        .flat_map(|s| s.sheets.iter())
        .flat_map(|s| s.nets.iter())
        .find(|net| net.name == n.name)
        .map(|net| net.class.clone())
        .or_else(|| {
            d.brd
                .as_ref()?
                .signals
                .iter()
                .find(|s| s.name == n.name)
                .map(|s| s.class.clone())
        });
    if let Some(class) = class.filter(|c| !c.is_empty() && c != "0") {
        v.insert("class".into(), json!(class));
    }
    let mut pins: Vec<(String, Value)> = d
        .pins
        .iter()
        .filter(|p| p.net == Some(id))
        .map(|p| {
            let part = &d.parts[p.part as usize].name;
            let key = format!("{part}.{}", p.pin);
            let mut row = Map::new();
            row.insert("part".into(), json!(part));
            row.insert("pin".into(), json!(p.pin));
            if !p.pads.is_empty() {
                row.insert("pads".into(), json!(p.pads.join(" ")));
            }
            row.insert("sheet".into(), json!(p.sheet + 1));
            row.insert("x".into(), round(p.at.x));
            row.insert("y".into(), round(p.at.y));
            (key, Value::Object(row))
        })
        .collect();
    pins.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    if n.in_sch {
        let sheets: Vec<usize> = d.net_sheets(id).into_iter().map(|s| s + 1).collect();
        v.insert(
            "schematic".into(),
            json!({"sheets": sheets, "pins": pins.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}),
        );
    }
    if let Some(r) = routing(d, &n.name) {
        let mut pads: Vec<String> = d
            .pads
            .iter()
            .filter(|p| p.net == Some(id))
            .map(|p| format!("{}.{}", d.parts[p.part as usize].name, p.pad))
            .collect();
        pads.sort_by(|a, b| natural_cmp(a, b));
        let tracks: Map<String, Value> = r
            .length
            .iter()
            .map(|(layer, len)| (layer_name(d, *layer).to_string(), round(*len)))
            .collect();
        let mut board = Map::new();
        board.insert("pads".into(), json!(pads));
        board.insert("track_mm".into(), Value::Object(tracks));
        board.insert("vias".into(), json!(r.vias));
        if !r.polygons.is_empty() {
            let polys: Vec<&str> = r.polygons.iter().map(|l| layer_name(d, *l)).collect();
            board.insert("polygons".into(), json!(polys));
        }
        board.insert("unrouted".into(), json!(r.airwires));
        v.insert("board".into(), Value::Object(board));
    }
    Ok(Value::Object(v))
}

fn find(d: &Design, text: &str) -> Value {
    let needle = text.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return json!({"parts": [], "nets": []});
    }
    let mut parts = Vec::new();
    for (i, p) in d.parts.iter().enumerate() {
        let id = i as u32;
        let value = d.value(id);
        let mut hits: Vec<String> = Vec::new();
        if p.name.to_ascii_lowercase().contains(&needle) {
            hits.push("name".into());
        }
        if value.to_ascii_lowercase().contains(&needle) {
            hits.push("value".into());
        }
        for (k, val) in d.attributes(id) {
            if val.to_ascii_lowercase().contains(&needle) {
                hits.push(k);
            }
        }
        if let Some(package) = d
            .element(id)
            .map(|e| e.package.clone())
            .or(d.sch_package(id).map(str::to_string))
            && package.to_ascii_lowercase().contains(&needle)
        {
            hits.push("package".into());
        }
        if !hits.is_empty() {
            parts.push(json!({"name": p.name, "value": value, "matched": hits.join(", ")}));
        }
    }
    let nets: Vec<&str> = d
        .nets
        .iter()
        .filter(|n| n.name.to_ascii_lowercase().contains(&needle))
        .map(|n| n.name.as_str())
        .collect();
    json!({"parts": parts, "nets": nets})
}

fn check(d: &Design) -> Value {
    let mut issues = Vec::new();
    let mut issue = |kind: &str, what: &str, detail: String| {
        issues.push(json!({"kind": kind, "item": what, "detail": detail}));
    };
    if d.sch.is_none() || d.brd.is_none() {
        issue(
            "files",
            &d.name,
            format!(
                "only the {} is there: nothing to compare",
                if d.sch.is_some() { "schematic" } else { "board" }
            ),
        );
    }
    let both = d.sch.is_some() && d.brd.is_some();
    for (i, p) in d.parts.iter().enumerate() {
        let id = i as u32;
        if !both {
            break;
        }
        match (d.sch_part(id), d.element(id)) {
            (Some(_), None) => {
                if let Some(package) = d.sch_package(id) {
                    issue(
                        "missing_on_board",
                        &p.name,
                        format!("{} ({package}) is in the schematic only", d.value(id)),
                    );
                }
            }
            (None, Some(e)) => issue(
                "missing_in_schematic",
                &p.name,
                format!("{} ({}) is on the board only", e.value, e.package),
            ),
            (Some(_), Some(e)) => {
                let value = d.value(id);
                // Eagle writes no board value for parts whose value comes from their device
                let derived = d.sch_part(id).is_some_and(|p| p.value.is_none()) && e.value.is_empty();
                if value != e.value && !derived {
                    issue("value", &p.name, format!("schematic {value:?}, board {:?}", e.value));
                }
                if let Some(package) = d.sch_package(id)
                    && package != e.package
                {
                    issue("package", &p.name, format!("schematic {package}, board {}", e.package));
                }
            }
            (None, None) => {}
        }
    }
    if both {
        // pin vs pad nets
        let pad_nets: BTreeMap<(u32, &str), Option<u32>> =
            d.pads.iter().map(|p| ((p.part, p.pad.as_str()), p.net)).collect();
        let mut seen = BTreeSet::new();
        for pin in &d.pins {
            if d.element(pin.part).is_none() {
                continue;
            }
            for pad in &pin.pads {
                seen.insert((pin.part, pad.as_str()));
                let Some(&brd_net) = pad_nets.get(&(pin.part, pad.as_str())) else {
                    continue;
                };
                if brd_net != pin.net {
                    let name = format!("{}.{}", d.parts[pin.part as usize].name, pad);
                    let show = |n: Option<u32>| n.map_or("(none)".to_string(), |n| d.nets[n as usize].name.clone());
                    issue(
                        "connection",
                        &name,
                        format!(
                            "pin {}: schematic net {}, board net {}",
                            pin.pin,
                            show(pin.net),
                            show(brd_net)
                        ),
                    );
                }
            }
        }
        for n in &d.nets {
            if let Some(r) = routing(d, &n.name)
                && r.airwires > 0
            {
                let detail = if r.polygons.is_empty() {
                    format!("{} airwire(s): connections left to route", r.airwires)
                } else {
                    format!(
                        "{} airwire(s): left to route, or joined by a polygon that was not poured when the file was saved",
                        r.airwires
                    )
                };
                issue("unrouted", &n.name, detail);
            }
        }
    }
    let count = issues.len();
    json!({"issues": issues, "count": count})
}

// ------------------------------------------------------------- rendering

struct RenderOptions {
    out: String,
    view: String,
    select: Option<String>,
    sheet: Option<usize>,
    width: u32,
    height: u32,
    actions: Vec<Action>,
}

fn render_command(design: &Arc<Design>, args: &[&str]) -> Result<()> {
    let mut options = RenderOptions {
        out: String::new(),
        view: "both".into(),
        select: None,
        sheet: None,
        width: 1600,
        height: 900,
        actions: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(&arg) = it.next() {
        match arg {
            "--view" => options.view = it.next().context("--view sch|brd|both")?.to_string(),
            "--select" => options.select = Some(it.next().context("--select NAME")?.to_string()),
            "--sheet" => options.sheet = Some(it.next().context("--sheet N")?.parse::<usize>()?.max(1) - 1),
            "--size" => {
                let size = it.next().context("--size WxH")?;
                let (w, h) = size.split_once('x').context("--size looks like 1600x900")?;
                options.width = w.parse()?;
                options.height = h.parse()?;
            }
            "--actions" => {
                for spec in it
                    .next()
                    .context("--actions needs a list")?
                    .split(';')
                    .filter(|s| !s.trim().is_empty())
                {
                    options.actions.push(Action::parse(spec.trim())?);
                }
            }
            _ if arg.starts_with('-') => bail!("unknown option {arg}"),
            _ => options.out = arg.to_string(),
        }
    }
    if options.out.is_empty() {
        bail!("render OUT.png");
    }
    let selection = match &options.select {
        Some(name) => match design.find(name) {
            Selection::None => bail!("nothing named {name:?}"),
            s => s,
        },
        None => Selection::None,
    };
    match options.view.as_str() {
        "sch" | "schematic" | "brd" | "board" => {
            let kind = if options.view.starts_with('s') {
                Kind::Schematic
            } else {
                Kind::Board
            };
            let scene = match kind {
                Kind::Schematic => {
                    // the sheet asked for, else the first one showing the selection
                    let sheet = options.sheet.unwrap_or_else(|| match selection {
                        Selection::Part(p) => design.sch_locations(p).first().map_or(0, |(s, _)| *s),
                        Selection::Net(n) => design.net_sheets(n).first().copied().unwrap_or(0),
                        Selection::None => 0,
                    });
                    design.sheets.get(sheet).context("no such sheet")?
                }
                Kind::Board => design.board.as_ref().context("no board")?,
            };
            let mut view = Viewport::fit(scene.bounds, options.width, options.height);
            let rect = scene.selection_bounds(selection);
            if !rect.is_empty() {
                let min = if kind == Kind::Schematic { 40.0 } else { 10.0 };
                let mut target = rect;
                target.union(Rect::around(
                    rect.center(),
                    (rect.width().max(rect.height()) * 0.5).max(min * 0.5),
                ));
                view.fit_rect(target, 0.06);
            }
            let pixmap = render::render(scene, &view, selection);
            pixmap
                .save_png(&options.out)
                .with_context(|| format!("writing {}", options.out))?;
        }
        "both" | "ui" => screenshot(design, &options, selection)?,
        other => bail!("--view {other}: sch, brd or both"),
    }
    eprintln!("[bligle] wrote {}", options.out);
    Ok(())
}

/// a scripted input for UI screenshots
#[derive(Clone, Debug)]
enum Action {
    Click(f32, f32),
    DoubleClick(f32, f32),
    Drag(f32, f32, f32, f32),
    /// positive notches zoom in
    Wheel(f32, f32, f32),
    Key(Key),
    Text(String),
}

impl Action {
    fn parse(spec: &str) -> Result<Action> {
        let (name, args) = spec.split_once(':').unwrap_or((spec, ""));
        let numbers = |count: usize| -> Result<Vec<f32>> {
            let values: Vec<f32> = args
                .split(',')
                .map(|n| n.trim().parse::<f32>().with_context(|| format!("bad number in {spec}")))
                .collect::<Result<_>>()?;
            anyhow::ensure!(values.len() == count, "{spec}: expected {count} numbers");
            Ok(values)
        };
        Ok(match name {
            "click" => {
                let v = numbers(2)?;
                Action::Click(v[0], v[1])
            }
            "dclick" => {
                let v = numbers(2)?;
                Action::DoubleClick(v[0], v[1])
            }
            "drag" => {
                let v = numbers(4)?;
                Action::Drag(v[0], v[1], v[2], v[3])
            }
            "wheel" => {
                let v = numbers(3)?;
                Action::Wheel(v[0], v[1], v[2])
            }
            "key" => Action::Key(match args {
                "esc" => Key::Escape,
                "enter" => Key::Enter,
                "pgdn" => Key::PageDown,
                "pgup" => Key::PageUp,
                "bksp" => Key::Backspace,
                other => Key::Character(other.chars().next().context("key needs a character")?),
            }),
            "text" => Action::Text(args.to_string()),
            _ => bail!("unknown action {spec}"),
        })
    }

    /// the inputs the desktop runner would send
    fn inputs(&self) -> Vec<Input<f32>> {
        let at = |x: f32, y: f32| LogicalPoint { x, y };
        let moved = |x, y| Input::MouseMove {
            position: at(x, y),
            modifiers: Modifiers::NONE,
        };
        let down = |x, y| Input::MouseDown {
            position: at(x, y),
            button: MouseButton::Primary,
            modifiers: Modifiers::NONE,
        };
        let up = |x, y| Input::MouseUp {
            position: at(x, y),
            button: MouseButton::Primary,
            modifiers: Modifiers::NONE,
            leave: false,
        };
        match *self {
            Action::Click(x, y) => vec![moved(x, y), down(x, y), up(x, y)],
            Action::DoubleClick(x, y) => vec![moved(x, y), down(x, y), up(x, y), down(x, y), up(x, y)],
            Action::Drag(x0, y0, x1, y1) => {
                let mut inputs = vec![moved(x0, y0), down(x0, y0)];
                for i in 1..=8 {
                    let t = i as f32 / 8.0;
                    inputs.push(moved(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
                }
                inputs.push(up(x1, y1));
                inputs
            }
            Action::Wheel(x, y, notches) => vec![
                moved(x, y),
                Input::Scroll {
                    position: at(x, y),
                    delta_x: 0.0,
                    delta_y: -notches * 40.0,
                    modifiers: Modifiers::NONE,
                    continuous: false,
                    phase: ScrollPhase::Moved,
                },
            ],
            Action::Key(key) => {
                let mut inputs = vec![Input::Key(KeyInput::new(key))];
                if let Key::Character(c) = key {
                    inputs.push(Input::Text(c));
                }
                inputs.push(Input::Key(KeyInput {
                    pressed: false,
                    ..KeyInput::new(key)
                }));
                inputs
            }
            Action::Text(ref text) => text.chars().map(Input::Text).collect(),
        }
    }
}

type Screen = Rc<RefCell<Renderer<VecBuffer<Xrgb8888>>>>;

/// Blit's CPU renderer drawing into memory, as the UI's graphics backend
struct Offscreen(Screen);

impl GraphicsBackend for Offscreen {
    fn create_image(&mut self, data: ImageData) -> Result<ImageHandle, GraphicsError> {
        Ok(self.0.borrow_mut().create_image(data))
    }

    fn resume(&mut self, _: Arc<winit::window::Window>) -> Result<(), GraphicsError> {
        Ok(())
    }

    fn suspend(&mut self) {}

    fn resize(&mut self, _: winit::dpi::PhysicalSize<u32>) -> Result<(), GraphicsError> {
        Ok(())
    }

    fn render(&mut self, input: RenderInput<'_>) -> Result<bool, GraphicsError> {
        self.0.borrow_mut().render(input, 1);
        Ok(true)
    }
}

/// renders the whole UI without a window, after the scripted actions
fn screenshot(design: &Arc<Design>, options: &RenderOptions, selection: Selection) -> Result<()> {
    let (width, height) = (options.width, options.height);
    let mut app = App::new(design.clone(), Arc::new(AtomicU32::new(1.0f32.to_bits())));
    if let Some(sheet) = options.sheet {
        app.set_sheet(sheet);
    }
    if let Some(name) = design.selection_name(selection) {
        app.select_named(name);
    }
    let text = TextSystem::new(crate::text_config(), blit_text_parley::Backend::new())
        .map_err(|error| anyhow::anyhow!("fonts: {error}"))?;
    let mut gui = GuiContext::new(text);
    gui.set_scale(1.0);
    let screen: Screen = Rc::new(RefCell::new(Renderer::new(
        VecBuffer::<Xrgb8888>::new(width as usize, height as usize),
        RendererConfig::default(),
    )));
    let mut context = DesktopContext::new(gui, Box::new(Offscreen(screen.clone())));
    let mut frame: Frame<DesktopContext> = Frame::default();
    let info = FrameInfo::new(Size::new(width as f32, height as f32));
    let start = Instant::now();
    let mut step = |input: Input<f32>| {
        frame.build(&mut context, info, start.elapsed(), input, |ui: Ui<'_>| app.render(ui));
        frame.layout(&mut context);
        frame.paint(&mut context);
        let DesktopContext { gui, graphics, .. } = &mut context;
        let _ = graphics.render(gui.render_input());
        gui.finish_frame();
    };
    for _ in 0..4 {
        step(Input::None);
    }
    for action in &options.actions {
        for input in action.inputs() {
            step(input);
        }
        for _ in 0..3 {
            step(Input::None);
        }
    }
    drop(step);
    let renderer = screen.borrow();
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("screenshot size")?;
    for (dst, pixel) in pixmap
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(renderer.buffer().pixels())
    {
        let raw = pixel.raw();
        dst.copy_from_slice(&[(raw >> 16) as u8, (raw >> 8) as u8, raw as u8, 255]);
    }
    pixmap
        .save_png(&options.out)
        .with_context(|| format!("writing {}", options.out))?;
    Ok(())
}

// ---------------------------------------------------------- text output

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("-".into()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        Value::Array(a) if a.iter().all(|v| scalar(v).is_some() && !v.is_array() && !v.is_object()) => {
            Some(a.iter().filter_map(scalar).collect::<Vec<_>>().join(", "))
        }
        Value::Object(m)
            if m.values()
                .all(|v| matches!(v, Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)))
                && m.len() <= 6 =>
        {
            Some(
                m.iter()
                    .map(|(k, v)| format!("{k}={}", scalar(v).unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        _ => None,
    }
}

/// a compact, indented text form of a JSON value
fn human(out: &mut String, v: &Value, indent: usize) {
    let pad = "  ".repeat(indent);
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                match (val, scalar(val)) {
                    (Value::Array(a), _) if a.is_empty() => out.push_str(&format!("{pad}{k}: (none)\n")),
                    (Value::Object(_), Some(s)) if indent > 0 || s.len() < 80 => {
                        out.push_str(&format!("{pad}{k}: {s}\n"))
                    }
                    (Value::Object(_), _) => {
                        out.push_str(&format!("{pad}{k}:\n"));
                        human(out, val, indent + 1);
                    }
                    (_, Some(s)) => out.push_str(&format!("{pad}{k}: {s}\n")),
                    _ => {
                        out.push_str(&format!("{pad}{k}:\n"));
                        human(out, val, indent + 1);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(map) => {
                        let line: Vec<String> = map
                            .iter()
                            .map(|(k, v)| match scalar(v) {
                                Some(s) => format!("{k}: {s}"),
                                None => format!("{k}: {v}"),
                            })
                            .collect();
                        out.push_str(&format!("{pad}- {}\n", line.join(" | ")));
                    }
                    other => out.push_str(&format!(
                        "{pad}- {}\n",
                        scalar(other).unwrap_or_else(|| other.to_string())
                    )),
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other).unwrap_or_default())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(split_words("part U1 --json"), ["part", "U1", "--json"]);
        assert_eq!(split_words("find \"100 nF\""), ["find", "100 nF"]);
    }
}
