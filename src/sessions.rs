//! recent projects, so the viewer reopens the last one and switches between them: each
//! remembers its sheet, selection and views. One line per project, most recent first, in
//! `$XDG_STATE_HOME/bligle/sessions` (`~/.local/state/bligle/sessions`).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    design::{self, Design},
    geom::Point,
};

/// projects kept in the list
const KEEP: usize = 30;

/// where a canvas looked
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub center: Point,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// the project's .sch (else its .brd), absolute
    pub path: PathBuf,
    /// unix seconds
    pub opened: u64,
    pub sheet: usize,
    /// `part:NAME` or `net:NAME`
    pub selection: Option<String>,
    /// schematic, board
    pub views: [Option<View>; 2],
}

pub struct Sessions {
    /// most recent first
    pub list: Vec<Session>,
    /// None: never written (headless screenshots read the list but leave it alone)
    file: Option<PathBuf>,
}

/// a project to show in the list
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub folder: String,
    pub sch: bool,
    pub brd: bool,
    /// None: not opened yet (found next to a recent project)
    pub opened: Option<u64>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn state_file() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(dir.join("bligle/sessions"))
}

/// the key of a project in the list: its .sch, else its .brd, absolute
pub fn key(design: &Design) -> Option<PathBuf> {
    let path = design.sch_path.as_ref().or(design.brd_path.as_ref())?;
    path.canonicalize().ok()
}

/// `~/x` and the home folder shown as `~`
pub fn expand(text: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(text),
    }
}

fn tilde(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display()),
        _ => path.display().to_string(),
    }
    .trim_end_matches('/')
    .to_string()
}

/// "5 min ago"
pub fn ago(opened: u64) -> String {
    let s = now().saturating_sub(opened);
    match s {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".to_string(),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn view_text(view: Option<View>) -> String {
    match view {
        Some(v) => format!("{},{},{}", v.center.x, v.center.y, v.scale),
        None => "-".to_string(),
    }
}

fn parse_view(text: &str) -> Option<View> {
    let mut it = text.split(',').map(|n| n.parse::<f64>().ok());
    let (x, y, scale) = (it.next()??, it.next()??, it.next()??);
    (scale.is_finite() && scale > 0.0 && x.is_finite() && y.is_finite()).then_some(View {
        center: Point::new(x, y),
        scale,
    })
}

fn parse_line(line: &str) -> Option<Session> {
    // the path goes last, so nothing it holds can shift the other fields
    let mut fields = line.splitn(6, '\t');
    let opened = fields.next()?.parse().ok()?;
    let sheet = fields.next()?.parse().ok()?;
    let selection = Some(fields.next()?.to_string()).filter(|s| !s.is_empty());
    let sch = parse_view(fields.next()?);
    let brd = parse_view(fields.next()?);
    let path = PathBuf::from(fields.next()?);
    path.is_absolute().then_some(Session {
        path,
        opened,
        sheet,
        selection,
        views: [sch, brd],
    })
}

fn line(s: &Session) -> Option<String> {
    let path = s.path.to_str()?;
    let selection = s.selection.as_deref().unwrap_or("");
    if path.contains('\n') || selection.contains(['\t', '\n']) {
        return None;
    }
    Some(format!(
        "{}\t{}\t{}\t{}\t{}\t{}\n",
        s.opened,
        s.sheet,
        selection,
        view_text(s.views[0]),
        view_text(s.views[1]),
        path
    ))
}

impl Sessions {
    /// the saved list; `persist` lets `save` write it back
    pub fn load(persist: bool) -> Sessions {
        Sessions::from_file(state_file(), persist)
    }

    pub fn from_file(file: Option<PathBuf>, persist: bool) -> Sessions {
        let mut list: Vec<Session> = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|text| text.lines().filter_map(parse_line).collect())
            .unwrap_or_default();
        list.sort_by_key(|s| std::cmp::Reverse(s.opened));
        let mut seen = HashSet::new();
        list.retain(|s| seen.insert(s.path.clone()));
        Sessions {
            list,
            file: file.filter(|_| persist),
        }
    }

    pub fn save(&self) {
        let Some(file) = &self.file else { return };
        let text: String = self.list.iter().take(KEEP).filter_map(line).collect();
        let result = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(file, text));
        if let Err(error) = result {
            eprintln!("[bligle] saving {}: {error}", file.display());
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Session> {
        self.list.iter().find(|s| s.path == path)
    }

    /// stores a project's state, in place (or first when it is new)
    pub fn update(&mut self, session: Session) {
        match self.list.iter_mut().find(|s| s.path == session.path) {
            Some(slot) => *slot = session,
            None => self.list.insert(0, session),
        }
    }

    /// moves a project to the top, opened now
    pub fn touch(&mut self, path: &Path) {
        let mut session = match self.list.iter().position(|s| s.path == path) {
            Some(i) => self.list.remove(i),
            None => Session {
                path: path.to_path_buf(),
                opened: 0,
                sheet: 0,
                selection: None,
                views: [None, None],
            },
        };
        session.opened = now();
        self.list.insert(0, session);
    }

    /// the recent projects that still exist, then the other projects in their folders
    /// (and in `folders`, first)
    pub fn entries(&self, folders: &[PathBuf]) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for s in &self.list {
            let Ok((sch, brd)) = design::project_files(&s.path) else {
                continue;
            };
            seen.insert(s.path.clone());
            entries.push(entry(&s.path, sch.is_some(), brd.is_some(), Some(s.opened)));
        }
        let mut dirs: Vec<PathBuf> = folders.to_vec();
        for e in &entries {
            if let Some(dir) = e.path.parent()
                && !dirs.iter().any(|d| d == dir)
            {
                dirs.push(dir.to_path_buf());
            }
        }
        for dir in dirs {
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            let mut stems: Vec<PathBuf> = read
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("sch") || e.eq_ignore_ascii_case("brd"))
                })
                .map(|p| p.with_extension(""))
                .collect();
            stems.sort_by(|a, b| design::natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
            stems.dedup();
            for stem in stems {
                let Ok((sch, brd)) = design::project_files(&stem) else {
                    continue;
                };
                let Some(path) = sch.as_ref().or(brd.as_ref()).and_then(|p| p.canonicalize().ok()) else {
                    continue;
                };
                if seen.insert(path.clone()) {
                    entries.push(entry(&path, sch.is_some(), brd.is_some(), None));
                }
            }
        }
        entries
    }
}

fn entry(path: &Path, sch: bool, brd: bool, opened: Option<u64>) -> Entry {
    Entry {
        path: path.to_path_buf(),
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        folder: path.parent().map(tilde).unwrap_or_default(),
        sch,
        brd,
        opened,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_roundtrip() {
        let s = Session {
            path: PathBuf::from("/a b/Pro\tject.sch"),
            opened: 1_700_000_000,
            sheet: 2,
            selection: Some("part:U1".into()),
            views: [
                Some(View {
                    center: Point::new(1.5, -2.25),
                    scale: 7.0,
                }),
                None,
            ],
        };
        let text = line(&s).unwrap();
        assert_eq!(parse_line(text.trim_end_matches('\n')), Some(s));
        assert_eq!(parse_line("garbage"), None);
        assert_eq!(parse_line("1\t0\t\t-\t-\trelative.sch"), None);
    }

    #[test]
    fn touch_and_update() {
        let mut sessions = Sessions {
            list: Vec::new(),
            file: None,
        };
        sessions.touch(Path::new("/a.sch"));
        sessions.touch(Path::new("/b.sch"));
        sessions.touch(Path::new("/a.sch"));
        let order: Vec<_> = sessions.list.iter().map(|s| s.path.to_str().unwrap()).collect();
        assert_eq!(order, ["/a.sch", "/b.sch"]);
        let mut b = sessions.get(Path::new("/b.sch")).unwrap().clone();
        b.sheet = 3;
        sessions.update(b);
        assert_eq!(sessions.list[1].sheet, 3);
    }
}
