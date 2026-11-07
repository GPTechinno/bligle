# bligle

**bl**it + ea**gle**: a viewer for Eagle schematics (`.sch`) and boards (`.brd`),
side by side in one native window built with the
[Blit](https://github.com/nicoburniske/blit) UI toolkit. Select a part or a net on
either side and it is highlighted on both. View only, no editing.

```
bligle path/to/Project.sch        # or Project.brd, or just path/to/Project
```

The `.sch` and `.brd` with the same name load together (Eagle 6+ XML files embed their
libraries, so nothing else is needed). Either one alone opens too.

## Viewer

| input | action |
| --- | --- |
| click | select the part or net under the pointer (a wire / track / via picks its net) |
| double-click | select and zoom both sides to it |
| drag, right / middle drag, touchpad | pan |
| wheel, ctrl + touchpad | zoom at the pointer |
| `/` or the search field | find a part or net by name, Enter selects it |
| `F` / `Z` | fit everything / zoom to the selection |
| PgUp / PgDn | previous / next sheet |
| Esc | clear the selection |

When you select something on one side, the other side brings it into view, switching
to the right sheet. The bar at the bottom describes the selection (value, package,
position and side on the board, sheet, description).

## Headless mode (for scripts and AI agents)

The same binary answers questions about a design without opening a window. Add
`--json` for machine-readable output; the default text output is compact and readable.

```
bligle <project> info                 # files, sheets, part/net counts, board size, layers
bligle <project> parts [FILTER]       # every part: value, package, sheet, board x/y/rot/side
bligle <project> part U1              # attributes, and each pad: pin, net, position, smd/tht
bligle <project> nets [FILTER]        # every net: pins, pads, sheets, track length, unrouted
bligle <project> net GND              # pins (with sheet / position), pads, track mm per layer, vias
bligle <project> find 100nF           # parts and nets matching names, values, attributes
bligle <project> check                # schematic vs board: missing parts, value / package
                                      # mismatches, pin vs pad nets, airwires
bligle <project> render out.png --view brd --select U1 --size 1600x900
                                      # PNG of the board (or --view sch) zoomed on U1
bligle <project> render out.png --select GND
                                      # PNG of the whole UI (both sides, like the window)
bligle <project> render out.png --actions 'click:1340,440;wheel:420,560,3;key:/;text:C12;key:enter'
                                      # the UI after scripted input (click, dclick, drag,
                                      # wheel, key, text), to check interactions
bligle <project> shell                # read commands from stdin, one per line
```

`shell` keeps the design loaded and runs one command per line (`part U1`, `net GND --json`,
…), printing a blank line after each answer: handy for an agent asking many questions.

Positions are millimetres in Eagle's coordinates (y up). `render` uses Blit's CPU
renderer offscreen for the whole UI, so it works without a display.

## Building

```
cargo build --release
cargo test --release
```

Blit uses a few nightly-only Rust features; `.cargo/config.toml` sets `RUSTC_BOOTSTRAP=1`
so the stable toolchain builds it. On Linux the window runs on Wayland and Blit loads
`libwayland-client`, `libxkbcommon` and `fontconfig` at runtime: the devenv in the
parent folder puts them on `LD_LIBRARY_PATH`. Headless commands need none of them.

## How it works

- `eagle.rs` reads the XML into plain data (libraries, parts, instances, nets, elements,
  signals, design rules).
- `design.rs` matches the two sides (Eagle keeps the same part and net names in both),
  and flattens each sheet and the board into world-space items tagged with their part
  and net; selection, highlighting and picking all work from those tags.
- `render.rs` draws a scene with tiny-skia (text from system font outlines, `text.rs`).
- `app.rs` is the Blit UI; each canvas re-renders its image when its view or the
  selection changes.
- `headless.rs` holds the commands and the offscreen UI screenshots.

## Limitations

- Polygon pours are shown as a light tint of their outline: Eagle does not store the
  poured copper in the file.
- Text uses a system sans-serif font, not Eagle's vector font.
- Busses are drawn but not selectable; implicit supply connections (hidden power pins)
  are not resolved.
- Older binary (pre-6) Eagle files are not supported.
