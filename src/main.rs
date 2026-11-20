//! bligle: an Eagle schematic and board viewer built on Blit. The schematic and the
//! board show side by side; selecting a part or a net in one highlights it in both.
//! A headless mode answers questions about a design and renders it to PNG, for
//! people and AI agents.

mod app;
mod design;
mod eagle;
mod geom;
mod headless;
mod render;
mod sessions;
mod text;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use blit_desktop::{Application, Config, DesktopContext, EventLoopProxy, GraphicsBackend, GraphicsError, Root, Ui};
use blit_gui::{
    Family, FontData, FontStack, RenderInput, TextConfig,
    image::{ImageData, ImageHandle},
    text::FontId,
};

use crate::{app::App, design::Design, sessions::Sessions};

const USAGE: &str = "\
bligle - Eagle schematic and board viewer, side by side

usage:
  bligle [project.sch | project.brd | project] [--select NAME] [--sheet N] [--size WxH]
      opens the viewer; the .sch and .brd of the same name load together. Without a
      project it reopens the last one; Ctrl+O lists the recent projects to switch to
  bligle <project> <command> [args] [--json]
      headless: answers about the design without opening a window

";

struct Options {
    select: Option<String>,
    sheet: Option<usize>,
    width: u32,
    height: u32,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        print!("{USAGE}{}", headless::COMMANDS);
        return Ok(());
    }
    // the project, unless only options follow (then the last project reopens)
    let (design, rest) = match args.first() {
        Some(first) if !first.starts_with('-') => (Some(Arc::new(Design::open(&PathBuf::from(first))?)), &args[1..]),
        _ => (None, &args[..]),
    };
    if let Some(design) = &design
        && rest.first().is_some_and(|a| !a.starts_with('-'))
    {
        return headless::run(design, rest);
    }
    let mut options = Options {
        select: None,
        sheet: None,
        width: 1600,
        height: 900,
    };
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--select" => options.select = Some(it.next().context("--select NAME")?.clone()),
            "--sheet" => options.sheet = Some(it.next().context("--sheet N")?.parse::<usize>()?.max(1) - 1),
            "--size" => {
                let size = it.next().context("--size WxH")?;
                let (w, h) = size.split_once('x').context("--size looks like 1600x900")?;
                options.width = w.parse()?;
                options.height = h.parse()?;
            }
            other => bail!("unknown option {other}\n\n{USAGE}{}", headless::COMMANDS),
        }
    }
    run_desktop(design, options)
}

/// UI fonts: the font the drawings use, then system fallbacks
pub fn text_config() -> TextConfig {
    let mut families = Vec::new();
    if let Some(font) = text::font() {
        families.push(Family::embedded([FontData::Shared(font.data.clone())]));
    }
    families.push(Family::named("DejaVu Sans"));
    families.push(Family::named("Noto Sans Symbols 2"));
    TextConfig {
        fonts: vec![FontStack {
            id: FontId::default(),
            families,
        }],
        text_cache_capacity: 1024 * 1024,
        layout_cache_capacity: 2 * 1024 * 1024,
    }
}

// ----------------------------------------------------------------- desktop

/// the window, which blit hands to the graphics backend only: kept to retitle it
type WindowSlot = Arc<Mutex<Option<Arc<winit::window::Window>>>>;

struct Startup {
    design: Option<Arc<Design>>,
    options: Options,
    scale: Arc<AtomicU32>,
    window: WindowSlot,
    title: String,
}

static STARTUP: Mutex<Option<Startup>> = Mutex::new(None);

struct Desktop {
    app: App,
    window: WindowSlot,
    title: String,
}

impl Application for Desktop {
    type Input = ();

    fn new(_: EventLoopProxy<()>, _: Root<Self>, _: &mut DesktopContext) -> Self {
        let startup = STARTUP.lock().unwrap().take().expect("startup state");
        let mut app = App::new(startup.scale, Sessions::load(true));
        match startup.design {
            Some(design) => app.show(design),
            None => app.open_last(),
        }
        if let Some(sheet) = startup.options.sheet {
            app.set_sheet(sheet);
        }
        if let Some(name) = &startup.options.select {
            app.select_named(name);
        }
        Desktop {
            app,
            window: startup.window,
            title: startup.title,
        }
    }

    fn input(&mut self, _: ()) {}

    fn render(&mut self, ui: Ui<'_>) {
        self.app.render(ui);
        // the title names the open project, which changes from the project list
        let title = self.app.title();
        if title != self.title
            && let Some(window) = self.window.lock().unwrap().as_ref()
        {
            window.set_title(&title);
            self.title = title;
        }
    }
}

/// passes everything to the real backend, noting the UI scale so the canvases render
/// at the physical resolution, and the window
struct ScaleProbe {
    inner: Box<dyn GraphicsBackend>,
    scale: Arc<AtomicU32>,
    window: WindowSlot,
}

impl GraphicsBackend for ScaleProbe {
    fn create_image(&mut self, data: ImageData) -> Result<ImageHandle, GraphicsError> {
        self.inner.create_image(data)
    }

    fn resume(&mut self, window: Arc<winit::window::Window>) -> Result<(), GraphicsError> {
        *self.window.lock().unwrap() = Some(window.clone());
        self.inner.resume(window)
    }

    fn suspend(&mut self) {
        self.inner.suspend()
    }

    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) -> Result<(), GraphicsError> {
        self.inner.resize(size)
    }

    fn render(&mut self, input: RenderInput<'_>) -> Result<bool, GraphicsError> {
        self.scale.store(input.scale.x.to_bits(), Ordering::Relaxed);
        self.inner.render(input)
    }
}

fn run_desktop(design: Option<Arc<Design>>, options: Options) -> Result<()> {
    let scale = Arc::new(AtomicU32::new(1.0f32.to_bits()));
    let window = WindowSlot::default();
    // the project name joins once it is open (the Hyprland rule floats `bligle` titles)
    let title = match &design {
        Some(design) => format!("{} — bligle", design.name),
        None => "bligle".to_string(),
    };
    let (width, height) = (options.width, options.height);
    *STARTUP.lock().unwrap() = Some(Startup {
        design,
        options,
        scale: scale.clone(),
        window: window.clone(),
        title: title.clone(),
    });
    let graphics = Box::new(ScaleProbe {
        inner: Box::new(blit_desktop::cpu::Backend::new(blit_desktop::cpu::Config::default())),
        scale,
        window,
    });
    blit_desktop::run::<Desktop>(Config {
        title,
        width,
        height,
        text_config: text_config(),
        text: blit_text_parley::Backend::new(),
        graphics,
    })
    .map_err(|error| anyhow::anyhow!("{error}"))
}
