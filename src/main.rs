//! antpad - a tiny Windows 95/98 Notepad-style editor for the terminal.

use std::{
    fs,
    io::{self, BufWriter, Write},
    path::PathBuf,
};

use crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute, queue,
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

const UNDO_LIMIT: usize = 100;

// ---------------------------------------------------------------- menus ---

#[derive(Clone, Copy)]
enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    Exit,
    Undo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    FindNext,
    About,
    Sep,
}

type Item = (&'static str, &'static str, Cmd);

const MENUS: [(&str, &[Item]); 4] = [
    (
        "File",
        &[
            ("New", "Ctrl+N", Cmd::New),
            ("Open...", "Ctrl+O", Cmd::Open),
            ("Save", "Ctrl+S", Cmd::Save),
            ("Save As...", "F12", Cmd::SaveAs),
            ("-", "", Cmd::Sep),
            ("Exit", "Ctrl+Q", Cmd::Exit),
        ],
    ),
    (
        "Edit",
        &[
            ("Undo", "Ctrl+Z", Cmd::Undo),
            ("-", "", Cmd::Sep),
            ("Cut", "Ctrl+X", Cmd::Cut),
            ("Copy", "Ctrl+C", Cmd::Copy),
            ("Paste", "Ctrl+V", Cmd::Paste),
            ("-", "", Cmd::Sep),
            ("Select All", "Ctrl+A", Cmd::SelectAll),
        ],
    ),
    (
        "Search",
        &[
            ("Find...", "Ctrl+F", Cmd::Find),
            ("Find Next", "F3", Cmd::FindNext),
        ],
    ),
    ("Help", &[("About Antpad", "", Cmd::About)]),
];

fn menu_x(top: usize) -> usize {
    2 + MENUS[..top].iter().map(|m| m.0.len() + 2).sum::<usize>()
}

// ---------------------------------------------------------------- state ---

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    New,
    Open,
    Exit,
}

#[derive(Clone, Copy)]
enum PromptKind {
    Find,
}

#[derive(Clone, Copy)]
enum BrowseKind {
    Open,
    SaveAs(Option<Pending>),
}

/// State of the Open / Save As file browser dialog.
struct Browser {
    kind: BrowseKind,
    dir: PathBuf,
    entries: Vec<(String, bool)>, // (name, is_dir); ".." first, then dirs, then files
    sel: usize,
    scroll: usize,
    name: String,
    hidden: bool,
}

impl Browser {
    fn new(kind: BrowseKind, dir: PathBuf, name: String) -> Self {
        let mut b = Browser { kind, dir, entries: Vec::new(), sel: 0, scroll: 0, name, hidden: false };
        b.reload();
        b
    }

    fn reload(&mut self) {
        let mut ents: Vec<(String, bool)> = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if !self.hidden && n.starts_with('.') {
                    continue;
                }
                ents.push((n, e.path().is_dir()));
            }
        }
        ents.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        if self.dir.parent().is_some() {
            ents.insert(0, ("..".into(), true));
        }
        self.entries = ents;
        self.sel = 0;
        self.scroll = 0;
    }

    fn enter_dir(&mut self, name: &str) {
        if name == ".." {
            if let Some(parent) = self.dir.parent() {
                let came_from = self.dir.file_name().map(|s| s.to_string_lossy().into_owned());
                self.dir = parent.to_path_buf();
                self.reload();
                if let Some(old) = came_from {
                    if let Some(i) = self.entries.iter().position(|e| e.0 == old) {
                        self.sel = i;
                    }
                }
            }
        } else {
            self.dir = self.dir.join(name);
            self.reload();
        }
    }

    fn scroll_into_view(&mut self, page: usize) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        }
        if self.sel >= self.scroll + page {
            self.scroll = self.sel + 1 - page;
        }
    }
}

enum Mode {
    Edit,
    Menu { top: usize, item: usize },
    Prompt { kind: PromptKind, input: String },
    Browser(Browser),
    Confirm(Pending),
    Overwrite(PathBuf, Option<Pending>),
    Message(Vec<String>),
}

type Pos = (usize, usize); // (row, col) in chars

struct Editor {
    lines: Vec<Vec<char>>,
    cy: usize,
    cx: usize,
    want_x: usize,
    anchor: Option<Pos>,
    top: usize,
    left: usize,
    path: Option<PathBuf>,
    modified: bool,
    trailing_nl: bool,
    clipboard: String,
    osc: Option<String>,
    undo: Vec<(Vec<Vec<char>>, usize, usize)>,
    typing: bool,
    last_find: String,
    mode: Mode,
    status: String,
    quit: bool,
    w: usize,
    h: usize,
}

impl Editor {
    fn new(w: u16, h: u16) -> Self {
        Editor {
            lines: vec![Vec::new()],
            cy: 0,
            cx: 0,
            want_x: 0,
            anchor: None,
            top: 0,
            left: 0,
            path: None,
            modified: false,
            trailing_nl: true,
            clipboard: String::new(),
            osc: None,
            undo: Vec::new(),
            typing: false,
            last_find: String::new(),
            mode: Mode::Edit,
            status: String::new(),
            quit: false,
            w: w as usize,
            h: h as usize,
        }
    }

    fn text_height(&self) -> usize {
        self.h.saturating_sub(2).max(1)
    }

    // ------------------------------------------------------ document ops ---

    fn reset_doc(&mut self) {
        self.lines = vec![Vec::new()];
        self.cy = 0;
        self.cx = 0;
        self.want_x = 0;
        self.anchor = None;
        self.top = 0;
        self.left = 0;
        self.undo.clear();
        self.typing = false;
        self.modified = false;
        self.trailing_nl = true;
        self.path = None;
    }

    fn open_path(&mut self, path: PathBuf) {
        match fs::read(&path) {
            Ok(bytes) => {
                if bytes.contains(&0) {
                    self.mode = Mode::Message(vec!["This looks like a binary file.".into(), "It cannot be opened.".into()]);
                    return;
                }
                let mut text = String::from_utf8_lossy(&bytes)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\t', "    ");
                let trailing = text.ends_with('\n');
                if trailing {
                    text.pop();
                }
                let lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
                self.reset_doc();
                self.lines = lines;
                self.trailing_nl = trailing || bytes.is_empty();
                self.path = Some(path);
                self.status = "Opened".into();
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.reset_doc();
                self.path = Some(path);
                self.status = "New file".into();
            }
            Err(e) => {
                self.mode = Mode::Message(vec![format!("Cannot open {}", path.display()), e.to_string()]);
            }
        }
    }

    fn write_to(&mut self, path: PathBuf) -> bool {
        let mut s = self
            .lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        if self.trailing_nl && !s.is_empty() {
            s.push('\n');
        }
        match fs::write(&path, s) {
            Ok(()) => {
                self.path = Some(path);
                self.modified = false;
                self.status = "Saved".into();
                true
            }
            Err(e) => {
                self.mode = Mode::Message(vec![format!("Cannot save {}", path.display()), e.to_string()]);
                false
            }
        }
    }

    fn save(&mut self, then: Option<Pending>) {
        match self.path.clone() {
            Some(p) => {
                if self.write_to(p) {
                    if let Some(t) = then {
                        self.do_pending(t);
                    }
                }
            }
            None => self.open_browser(BrowseKind::SaveAs(then)),
        }
    }

    fn request(&mut self, p: Pending) {
        if self.modified {
            self.mode = Mode::Confirm(p);
        } else {
            self.do_pending(p);
        }
    }

    fn do_pending(&mut self, p: Pending) {
        match p {
            Pending::New => self.reset_doc(),
            Pending::Open => self.open_browser(BrowseKind::Open),
            Pending::Exit => self.quit = true,
        }
    }

    // ------------------------------------------------------ editing core ---

    fn push_undo(&mut self) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push((self.lines.clone(), self.cy, self.cx));
    }

    fn undo(&mut self) {
        if let Some((lines, cy, cx)) = self.undo.pop() {
            self.lines = lines;
            self.cy = cy;
            self.cx = cx;
            self.want_x = cx;
            self.anchor = None;
            self.modified = true;
        } else {
            self.status = "Nothing to undo".into();
        }
    }

    fn sel_range(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        let c = (self.cy, self.cx);
        if a == c {
            None
        } else if a < c {
            Some((a, c))
        } else {
            Some((c, a))
        }
    }

    fn selected_text(&self) -> Option<String> {
        let (s, e) = self.sel_range()?;
        let mut t = String::new();
        if s.0 == e.0 {
            t.extend(self.lines[s.0][s.1..e.1].iter());
        } else {
            t.extend(self.lines[s.0][s.1..].iter());
            for i in s.0 + 1..e.0 {
                t.push('\n');
                t.extend(self.lines[i].iter());
            }
            t.push('\n');
            t.extend(self.lines[e.0][..e.1].iter());
        }
        Some(t)
    }

    /// Removes the selection (if any). Caller is responsible for push_undo.
    fn delete_selection(&mut self) {
        if let Some((s, e)) = self.sel_range() {
            if s.0 == e.0 {
                self.lines[s.0].drain(s.1..e.1);
            } else {
                let tail = self.lines[e.0][e.1..].to_vec();
                self.lines[s.0].truncate(s.1);
                self.lines[s.0].extend(tail);
                self.lines.drain(s.0 + 1..=e.0);
            }
            self.cy = s.0;
            self.cx = s.1;
            self.modified = true;
        }
        self.anchor = None;
        self.want_x = self.cx;
    }

    fn insert_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        let rest = self.lines[self.cy].split_off(self.cx);
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        self.lines[self.cy].extend(first.chars());
        for p in parts {
            self.cy += 1;
            self.lines.insert(self.cy, p.chars().collect());
        }
        self.cx = self.lines[self.cy].len();
        self.lines[self.cy].extend(rest);
        self.want_x = self.cx;
        self.modified = true;
    }

    /// `group`: consecutive typed characters share one undo step.
    fn type_text(&mut self, text: &str, group: bool) {
        if !(group && self.typing) || self.anchor.is_some() {
            self.push_undo();
        }
        self.typing = group;
        self.delete_selection();
        self.insert_text(text);
    }

    fn backspace(&mut self) {
        if self.sel_range().is_some() {
            self.push_undo();
            self.delete_selection();
        } else if self.cx > 0 {
            self.push_undo();
            self.cx -= 1;
            self.lines[self.cy].remove(self.cx);
            self.modified = true;
        } else if self.cy > 0 {
            self.push_undo();
            let l = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.lines[self.cy].len();
            self.lines[self.cy].extend(l);
            self.modified = true;
        }
        self.anchor = None;
        self.want_x = self.cx;
    }

    fn delete(&mut self) {
        if self.sel_range().is_some() {
            self.push_undo();
            self.delete_selection();
        } else if self.cx < self.lines[self.cy].len() {
            self.push_undo();
            self.lines[self.cy].remove(self.cx);
            self.modified = true;
        } else if self.cy + 1 < self.lines.len() {
            self.push_undo();
            let l = self.lines.remove(self.cy + 1);
            self.lines[self.cy].extend(l);
            self.modified = true;
        }
        self.anchor = None;
    }

    fn copy(&mut self) {
        if let Some(t) = self.selected_text() {
            self.osc = Some(base64(t.as_bytes()));
            self.clipboard = t;
            self.status = "Copied".into();
        }
    }

    fn cut(&mut self) {
        if self.sel_range().is_some() {
            self.copy();
            self.push_undo();
            self.delete_selection();
        }
    }

    fn paste(&mut self) {
        if !self.clipboard.is_empty() {
            let t = self.clipboard.clone();
            self.type_text(&t, false);
        }
    }

    // ----------------------------------------------------------- movement ---

    fn mv(&mut self, shift: bool, f: impl FnOnce(&mut Self)) {
        if shift {
            if self.anchor.is_none() {
                self.anchor = Some((self.cy, self.cx));
            }
        } else {
            self.anchor = None;
        }
        f(self);
    }

    fn left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        } else if self.cy > 0 {
            self.cy -= 1;
            self.cx = self.lines[self.cy].len();
        }
        self.want_x = self.cx;
    }

    fn right(&mut self) {
        if self.cx < self.lines[self.cy].len() {
            self.cx += 1;
        } else if self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = 0;
        }
        self.want_x = self.cx;
    }

    fn up(&mut self, n: usize) {
        self.cy = self.cy.saturating_sub(n);
        self.cx = self.want_x.min(self.lines[self.cy].len());
    }

    fn down(&mut self, n: usize) {
        self.cy = (self.cy + n).min(self.lines.len() - 1);
        self.cx = self.want_x.min(self.lines[self.cy].len());
    }

    fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.cy = self.lines.len() - 1;
        self.cx = self.lines[self.cy].len();
        self.want_x = self.cx;
    }

    fn find_next(&mut self) {
        if self.last_find.is_empty() {
            self.mode = Mode::Prompt { kind: PromptKind::Find, input: String::new() };
            return;
        }
        let needle: Vec<char> = self.last_find.chars().map(fold).collect();
        let n = self.lines.len();
        for i in 0..=n {
            let row = (self.cy + i) % n;
            let start = if i == 0 { self.cx } else { 0 };
            let hay: Vec<char> = self.lines[row].iter().map(|&c| fold(c)).collect();
            if start > hay.len() {
                continue;
            }
            if let Some(p) = hay[start..].windows(needle.len()).position(|w| w == &needle[..]) {
                let col = start + p;
                self.anchor = Some((row, col));
                self.cy = row;
                self.cx = col + needle.len();
                self.want_x = self.cx;
                return;
            }
        }
        self.status = format!("Cannot find \"{}\"", self.last_find);
    }

    // ------------------------------------------------------------ commands ---

    fn run(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::New => self.request(Pending::New),
            Cmd::Open => self.request(Pending::Open),
            Cmd::Save => self.save(None),
            Cmd::SaveAs => self.open_browser(BrowseKind::SaveAs(None)),
            Cmd::Exit => self.request(Pending::Exit),
            Cmd::Undo => self.undo(),
            Cmd::Cut => self.cut(),
            Cmd::Copy => self.copy(),
            Cmd::Paste => self.paste(),
            Cmd::SelectAll => self.select_all(),
            Cmd::Find => {
                self.mode = Mode::Prompt { kind: PromptKind::Find, input: self.last_find.clone() };
            }
            Cmd::FindNext => self.find_next(),
            Cmd::About => {
                self.mode = Mode::Message(vec![
                    "Antpad 0.1".into(),
                    "A tiny Notepad-style editor for the terminal.".into(),
                ]);
            }
            Cmd::Sep => {}
        }
    }

    // --------------------------------------------------------------- input ---

    fn on_key(&mut self, k: KeyEvent) {
        self.status.clear();
        let mode = std::mem::replace(&mut self.mode, Mode::Edit);
        match mode {
            Mode::Edit => self.key_edit(k),
            Mode::Menu { top, item } => self.key_menu(top, item, k),
            Mode::Prompt { kind, input } => self.key_prompt(kind, input, k),
            Mode::Browser(b) => self.key_browser(b, k),
            Mode::Confirm(p) => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.save(Some(p)),
                KeyCode::Char('n') | KeyCode::Char('N') => self.do_pending(p),
                KeyCode::Esc => {}
                _ => self.mode = Mode::Confirm(p),
            },
            Mode::Overwrite(path, then) => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if self.write_to(path) {
                        if let Some(t) = then {
                            self.do_pending(t);
                        }
                    }
                }
                _ => {}
            },
            Mode::Message(_) => {}
        }
    }

    fn on_paste(&mut self, s: String) {
        match &mut self.mode {
            Mode::Edit => self.type_text(&s, false),
            Mode::Prompt { input, .. } => {
                input.extend(s.lines().next().unwrap_or("").chars().filter(|c| !c.is_control()))
            }
            Mode::Browser(b) => {
                b.name.extend(s.lines().next().unwrap_or("").chars().filter(|c| !c.is_control()))
            }
            _ => {}
        }
    }

    fn open_browser(&mut self, kind: BrowseKind) {
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let dir = fs::canonicalize(&dir).unwrap_or(dir);
        let name = match kind {
            BrowseKind::SaveAs(_) => self
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            BrowseKind::Open => String::new(),
        };
        self.mode = Mode::Browser(Browser::new(kind, dir, name));
    }

    fn key_browser(&mut self, mut b: Browser, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = browser_geom(self.w, self.h).3;
        let last = b.entries.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Up => b.sel = b.sel.saturating_sub(1),
            KeyCode::Down => b.sel = (b.sel + 1).min(last),
            KeyCode::PageUp => b.sel = b.sel.saturating_sub(page),
            KeyCode::PageDown => b.sel = (b.sel + page).min(last),
            KeyCode::Home => b.sel = 0,
            KeyCode::End => b.sel = last,
            KeyCode::F(4) => {
                b.hidden = !b.hidden;
                b.reload();
            }
            KeyCode::Enter => {
                self.browser_activate(b);
                return;
            }
            KeyCode::Backspace => {
                // empty name box: go up one directory
                if b.name.pop().is_none() {
                    b.enter_dir("..");
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => b.name.push(c),
            _ => {}
        }
        b.scroll_into_view(page);
        self.mode = Mode::Browser(b);
    }

    fn browser_activate(&mut self, mut b: Browser) {
        let name = b.name.trim().to_string();
        if !name.is_empty() {
            let typed = expand(&name);
            let p = if typed.is_absolute() { typed } else { b.dir.join(typed) };
            if p.is_dir() {
                b.dir = fs::canonicalize(&p).unwrap_or(p);
                b.name.clear();
                b.reload();
                self.mode = Mode::Browser(b);
            } else {
                self.browser_finish(b.kind, p);
            }
            return;
        }
        match b.entries.get(b.sel).cloned() {
            Some((n, true)) => {
                b.enter_dir(&n);
                self.mode = Mode::Browser(b);
            }
            Some((n, false)) => self.browser_finish(b.kind, b.dir.join(n)),
            None => self.mode = Mode::Browser(b),
        }
    }

    fn browser_finish(&mut self, kind: BrowseKind, p: PathBuf) {
        match kind {
            BrowseKind::Open => self.open_path(p),
            BrowseKind::SaveAs(then) => {
                if p.exists() && self.path.as_ref() != Some(&p) {
                    self.mode = Mode::Overwrite(p, then);
                } else if self.write_to(p) {
                    if let Some(t) = then {
                        self.do_pending(t);
                    }
                }
            }
        }
    }

    fn place_cursor(&mut self, x: usize, y: usize, extend: bool) {
        let th = self.text_height();
        let r = y.clamp(1, th) - 1;
        self.cy = (self.top + r).min(self.lines.len() - 1);
        self.cx = (self.left + x.saturating_sub(1)).min(self.lines[self.cy].len());
        self.want_x = self.cx;
        if !extend {
            self.anchor = None;
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column as usize, m.row as usize);
        let th = self.text_height();
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.status.clear();
                self.typing = false;
                match std::mem::replace(&mut self.mode, Mode::Edit) {
                    Mode::Menu { top, item } => {
                        if let Some(i) = dropdown_hit(self.w, top, x, y) {
                            let c = MENUS[top].1[i].2;
                            if matches!(c, Cmd::Sep) {
                                self.mode = Mode::Menu { top, item };
                            } else {
                                self.run(c);
                            }
                        } else if y == 0 {
                            // clicking another title switches menus; the same title closes it
                            if let Some(t) = menu_at(x) {
                                if t != top {
                                    self.mode = Mode::Menu { top: t, item: 0 };
                                }
                            }
                        }
                    }
                    Mode::Edit => {
                        if y == 0 {
                            if let Some(t) = menu_at(x) {
                                self.mode = Mode::Menu { top: t, item: 0 };
                            }
                        } else if y >= 1 && y <= th && x >= 1 && x + 1 < self.w {
                            self.place_cursor(x, y, false);
                        }
                    }
                    Mode::Browser(mut b) => {
                        let (bx, by, bw, lh) = browser_geom(self.w, self.h);
                        if x > bx && x + 1 < bx + bw && y >= by + 2 && y < by + 2 + lh {
                            let idx = b.scroll + (y - by - 2);
                            if idx < b.entries.len() {
                                if idx == b.sel {
                                    // second click on the selected row = activate it
                                    self.browser_activate(b);
                                    return;
                                }
                                b.sel = idx;
                            }
                        }
                        self.mode = Mode::Browser(b);
                    }
                    other => self.mode = other,
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if matches!(self.mode, Mode::Browser(_)) =>
            {
                let page = browser_geom(self.w, self.h).3;
                if let Mode::Browser(b) = &mut self.mode {
                    if m.kind == MouseEventKind::ScrollUp {
                        b.sel = b.sel.saturating_sub(3);
                    } else {
                        b.sel = (b.sel + 3).min(b.entries.len().saturating_sub(1));
                    }
                    b.scroll_into_view(page);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if matches!(self.mode, Mode::Edit) => {
                if self.anchor.is_none() {
                    self.anchor = Some((self.cy, self.cx));
                }
                self.place_cursor(x, y, true);
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if matches!(self.mode, Mode::Edit) =>
            {
                if m.kind == MouseEventKind::ScrollUp {
                    self.top = self.top.saturating_sub(3);
                } else {
                    self.top = (self.top + 3).min(self.lines.len() - 1);
                }
                // keep the cursor inside the view so it doesn't snap back
                if self.cy < self.top {
                    self.cy = self.top;
                } else if self.cy >= self.top + th {
                    self.cy = self.top + th - 1;
                }
                self.cx = self.want_x.min(self.lines[self.cy].len());
            }
            _ => {}
        }
    }

    fn key_edit(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let typing_key = matches!(k.code, KeyCode::Char(_)) && !ctrl && !alt;
        if !typing_key {
            self.typing = false;
        }
        let th = self.text_height();
        match k.code {
            KeyCode::Char(c) if alt && !ctrl => {
                let top = match c.to_ascii_lowercase() {
                    'f' => 0,
                    'e' => 1,
                    's' => 2,
                    'h' => 3,
                    _ => return,
                };
                self.mode = Mode::Menu { top, item: 0 };
            }
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'n' => self.run(Cmd::New),
                'o' => self.run(Cmd::Open),
                's' => self.run(Cmd::Save),
                'q' => self.run(Cmd::Exit),
                'z' => self.run(Cmd::Undo),
                'x' => self.run(Cmd::Cut),
                'c' => self.run(Cmd::Copy),
                'v' => self.run(Cmd::Paste),
                'a' => self.run(Cmd::SelectAll),
                'f' => self.run(Cmd::Find),
                _ => {}
            },
            KeyCode::Char(c) => self.type_text(&c.to_string(), true),
            KeyCode::Enter => self.type_text("\n", false),
            KeyCode::Tab => {
                let n = 4 - self.cx % 4;
                self.type_text(&" ".repeat(n), false);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.mv(shift, |s| s.left()),
            KeyCode::Right => self.mv(shift, |s| s.right()),
            KeyCode::Up => self.mv(shift, |s| s.up(1)),
            KeyCode::Down => self.mv(shift, |s| s.down(1)),
            KeyCode::PageUp => self.mv(shift, |s| s.up(th)),
            KeyCode::PageDown => self.mv(shift, |s| s.down(th)),
            KeyCode::Home => self.mv(shift, |s| {
                if ctrl {
                    s.cy = 0;
                }
                s.cx = 0;
                s.want_x = 0;
            }),
            KeyCode::End => self.mv(shift, |s| {
                if ctrl {
                    s.cy = s.lines.len() - 1;
                }
                s.cx = s.lines[s.cy].len();
                s.want_x = s.cx;
            }),
            KeyCode::Esc => self.anchor = None,
            KeyCode::F(10) => self.mode = Mode::Menu { top: 0, item: 0 },
            KeyCode::F(3) => self.find_next(),
            KeyCode::F(12) => self.run(Cmd::SaveAs),
            _ => {}
        }
    }

    fn key_menu(&mut self, top: usize, item: usize, k: KeyEvent) {
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let (mut top, mut item) = (top, item);
        let is_sep = |t: usize, i: usize| matches!(MENUS[t].1[i].2, Cmd::Sep);
        let n = MENUS[top].1.len();
        match k.code {
            KeyCode::Esc | KeyCode::F(10) => return,
            KeyCode::Left => {
                top = (top + MENUS.len() - 1) % MENUS.len();
                item = 0;
            }
            KeyCode::Right => {
                top = (top + 1) % MENUS.len();
                item = 0;
            }
            KeyCode::Up => loop {
                item = (item + n - 1) % n;
                if !is_sep(top, item) {
                    break;
                }
            },
            KeyCode::Down => loop {
                item = (item + 1) % n;
                if !is_sep(top, item) {
                    break;
                }
            },
            KeyCode::Enter => {
                let c = MENUS[top].1[item].2;
                self.run(c);
                return;
            }
            KeyCode::Char(c) if alt => {
                match c.to_ascii_lowercase() {
                    'f' => top = 0,
                    'e' => top = 1,
                    's' => top = 2,
                    'h' => top = 3,
                    _ => {}
                }
                item = 0;
            }
            _ => {}
        }
        self.mode = Mode::Menu { top, item };
    }

    fn key_prompt(&mut self, kind: PromptKind, mut input: String, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                self.finish_prompt(kind, input);
                return;
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char('u') if ctrl => input.clear(),
            KeyCode::Char(c) if !ctrl && !alt => input.push(c),
            _ => {}
        }
        self.mode = Mode::Prompt { kind, input };
    }

    fn finish_prompt(&mut self, kind: PromptKind, input: String) {
        match kind {
            PromptKind::Find => {
                if !input.is_empty() {
                    self.last_find = input;
                    self.find_next();
                }
            }
        }
    }

    // ----------------------------------------------------------- rendering ---

    fn adjust_scroll(&mut self) {
        let th = self.text_height();
        let w = self.w.saturating_sub(2).max(1);
        if self.cy < self.top {
            self.top = self.cy;
        }
        if self.cy >= self.top + th {
            self.top = self.cy + 1 - th;
        }
        if self.cx < self.left {
            self.left = self.cx;
        }
        if self.cx >= self.left + w {
            self.left = self.cx + 1 - w;
        }
    }

    fn draw<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        let (w, h) = (self.w, self.h);
        if let Some(b64) = self.osc.take() {
            // OSC 52: ask the terminal to put the text on the system clipboard.
            queue!(out, Print(format!("\x1b]52;c;{}\x07", b64)))?;
        }
        if w < 30 || h < 8 {
            queue!(out, cursor::Hide)?;
            put(out, 0, 0, &fit("Terminal too small", w), TXT)?;
            return out.flush();
        }
        self.adjust_scroll();
        queue!(out, cursor::Hide)?;

        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        let tw = w - 2;
        let th = self.text_height();

        // top border:  ╭─ File  Edit  Search  Help ───────┤ name ├─╮
        put(out, 0, 0, &format!("╭{}╮", "─".repeat(w - 2)), BORDER)?;
        let active = if let Mode::Menu { top, .. } = self.mode { Some(top) } else { None };
        for (i, m) in MENUS.iter().enumerate() {
            let x = menu_x(i);
            if active == Some(i) {
                put(out, x, 0, &format!(" {} ", m.0), SEL)?;
            } else {
                put(out, x, 0, " ", TXT)?;
                put(out, x + 1, 0, &m.0[..1], HOT)?;
                put(out, x + 2, 0, &m.0[1..], BOLD)?;
                put(out, x + m.0.len() + 1, 0, " ", TXT)?;
            }
        }
        let shown_name = format!("{}{}", name, if self.modified { "*" } else { "" });
        let nl = shown_name.chars().count();
        if w > menu_x(MENUS.len()) + nl + 6 {
            let x0 = w - 2 - (nl + 4);
            put(out, x0, 0, "┤ ", BORDER)?;
            put(out, x0 + 2, 0, &shown_name, BOLD)?;
            put(out, x0 + 2 + nl, 0, " ├", BORDER)?;
        }

        // text area, framed by │ borders
        let sel = self.sel_range();
        for r in 0..th {
            let y = 1 + r;
            let idx = self.top + r;
            let line: &[char] = self.lines.get(idx).map(|l| l.as_slice()).unwrap_or(&[]);
            let (a, b) = match sel {
                Some((s, e)) if idx >= s.0 && idx <= e.0 => (
                    if idx == s.0 { s.1 } else { 0 },
                    if idx == e.0 { e.1 } else { line.len() + 1 },
                ),
                _ => (1, 0),
            };
            put(out, 0, y, "│", BORDER)?;
            put(out, w - 1, y, "│", BORDER)?;
            queue!(out, cursor::MoveTo(1, y as u16))?;
            let mut buf = String::new();
            let mut cur_sel = false;
            style(out, TXT)?;
            for col in self.left..self.left + tw {
                let ch = line.get(col).copied().unwrap_or(' ');
                let ch = if ch.is_control() { '\u{b7}' } else { ch };
                let s = col >= a && col < b;
                if s != cur_sel {
                    queue!(out, Print(&buf))?;
                    buf.clear();
                    style(out, if s { SEL } else { TXT })?;
                    cur_sel = s;
                }
                buf.push(ch);
            }
            queue!(out, Print(&buf))?;
        }

        // bottom border:  ╰─┤ Ln 1, Col 1 ├────────┤ hints / status ├─╯
        put(out, 0, h - 1, &format!("╰{}╯", "─".repeat(w - 2)), BORDER)?;
        let pos = format!("Ln {}, Col {}", self.cy + 1, self.cx + 1);
        put(out, 2, h - 1, "┤ ", BORDER)?;
        put(out, 4, h - 1, &pos, BOLD)?;
        put(out, 4 + pos.len(), h - 1, " ├", BORDER)?;
        let (msg, mst) = if self.status.is_empty() {
            ("^S Save  ^O Open  ^F Find  ^Q Quit  F10 Menu".to_string(), DIM)
        } else {
            (self.status.clone(), BOLD)
        };
        let ml = msg.chars().count();
        let start = 4 + pos.len() + 2;
        if w >= start + ml + 7 {
            let x0 = w - 2 - (ml + 4);
            put(out, x0, h - 1, "┤ ", BORDER)?;
            put(out, x0 + 2, h - 1, &msg, mst)?;
            put(out, x0 + 2 + ml, h - 1, " ├", BORDER)?;
        }

        // overlays
        let mut cursor_at: Option<(usize, usize)> = None;
        match &self.mode {
            Mode::Edit => {
                cursor_at = Some((self.cx - self.left + 1, self.cy - self.top + 1));
            }
            Mode::Menu { top, item } => draw_dropdown(out, w, *top, *item)?,
            Mode::Prompt { kind, input } => {
                let (title, label) = match kind {
                    PromptKind::Find => ("Find", "Find what:"),
                };
                let lines = [label.to_string(), String::new()];
                let (x, y, inner) = draw_box(out, w, h, title, &lines, 44)?;
                let chars: Vec<char> = input.chars().collect();
                let skip = (chars.len() + 1).saturating_sub(inner);
                let shown: String = chars[skip..].iter().collect();
                put(out, x + 2, y + 3, &fit(&shown, inner), BOLD)?;
                cursor_at = Some((x + 2 + chars.len() - skip, y + 3));
            }
            Mode::Confirm(_) => {
                let lines = [
                    format!("Do you want to save changes to {}?", name),
                    String::new(),
                    "[Y] Yes    [N] No    [Esc] Cancel".to_string(),
                ];
                draw_box(out, w, h, "Notepad", &lines, 0)?;
            }
            Mode::Browser(b) => cursor_at = Some(draw_browser(out, w, h, b)?),
            Mode::Overwrite(p, _) => {
                let lines = [
                    format!("{} already exists.", p.display()),
                    "Do you want to replace it?".to_string(),
                    String::new(),
                    "[Y] Yes    [any other key] No".to_string(),
                ];
                draw_box(out, w, h, "Save As", &lines, 0)?;
            }
            Mode::Message(msg) => {
                let mut lines = msg.clone();
                lines.push(String::new());
                lines.push("Press any key".to_string());
                draw_box(out, w, h, "Notepad", &lines, 0)?;
            }
        }
        if let Some((x, y)) = cursor_at {
            if x < w {
                queue!(out, cursor::MoveTo(x as u16, y as u16), cursor::Show)?;
            }
        }
        out.flush()
    }
}

// ------------------------------------------------------------- helpers ---

fn fit(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat(' ').take(w - n));
    out
}

/// A text style. Backgrounds are never set, so the terminal's own background
/// and (for TXT) foreground show through; colours are ANSI so they follow the theme.
#[derive(Clone, Copy)]
struct St {
    fg: Color,
    bold: bool,
    rev: bool,
}

const TXT: St = St { fg: Color::Reset, bold: false, rev: false };
const BOLD: St = St { fg: Color::Reset, bold: true, rev: false };
const BORDER: St = St { fg: Color::Cyan, bold: false, rev: false };
const HOT: St = St { fg: Color::Red, bold: true, rev: false };
const DIM: St = St { fg: Color::DarkGrey, bold: false, rev: false };
const DIR: St = St { fg: Color::Blue, bold: true, rev: false };
const SEL: St = St { fg: Color::Reset, bold: false, rev: true };

fn style<W: Write>(out: &mut W, st: St) -> io::Result<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;
    if st.fg != Color::Reset {
        queue!(out, SetForegroundColor(st.fg))?;
    }
    if st.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if st.rev {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}

fn put<W: Write>(out: &mut W, x: usize, y: usize, text: &str, st: St) -> io::Result<()> {
    queue!(out, cursor::MoveTo(x as u16, y as u16))?;
    style(out, st)?;
    queue!(out, Print(text))
}

/// Rounded box with the title embedded in the top border, interior cleared.
fn draw_frame<W: Write>(out: &mut W, x: usize, y: usize, bw: usize, bh: usize, title: &str) -> io::Result<()> {
    let tl = title.chars().count();
    put(out, x, y, &format!("╭{}╮", "─".repeat(bw - 2)), BORDER)?;
    if tl + 6 <= bw {
        put(out, x + 2, y, "┤ ", BORDER)?;
        put(out, x + 4, y, title, BOLD)?;
        put(out, x + 4 + tl, y, " ├", BORDER)?;
    }
    for r in 1..bh - 1 {
        put(out, x, y + r, &format!("│{}│", " ".repeat(bw - 2)), BORDER)?;
    }
    put(out, x, y + bh - 1, &format!("╰{}╯", "─".repeat(bw - 2)), BORDER)
}

/// Draws a centered dialog. Returns (x, y, inner_width); content line `i`
/// is at row `y + 2 + i`, starting at column `x + 2`.
fn draw_box<W: Write>(
    out: &mut W,
    w: usize,
    h: usize,
    title: &str,
    lines: &[String],
    min_inner: usize,
) -> io::Result<(usize, usize, usize)> {
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let inner = widest
        .max(title.chars().count() + 2)
        .max(min_inner)
        .min(w.saturating_sub(4));
    let bw = inner + 4;
    let bh = lines.len() + 4;
    let x = (w - bw) / 2;
    let y = h.saturating_sub(bh) / 2;
    draw_frame(out, x, y, bw, bh, title)?;
    for (i, l) in lines.iter().enumerate() {
        put(out, x + 2, y + 2 + i, &fit(l, inner), TXT)?;
    }
    Ok((x, y, inner))
}

/// (x, label width, shortcut width, inner width) of a dropdown.
fn dropdown_geom(w: usize, top: usize) -> (usize, usize, usize, usize) {
    let items = MENUS[top].1;
    let lw = items.iter().map(|i| i.0.len()).max().unwrap_or(0);
    let sw = items.iter().map(|i| i.1.len()).max().unwrap_or(0);
    let inner = lw + 3 + sw;
    (menu_x(top).min(w.saturating_sub(inner + 4)), lw, sw, inner)
}

/// Which dropdown item (if any) is under the screen cell (x, y).
/// The dropdown's top border is on row 1, items start on row 2.
fn dropdown_hit(w: usize, top: usize, x: usize, y: usize) -> Option<usize> {
    let (x0, _, _, inner) = dropdown_geom(w, top);
    if x >= x0 && x < x0 + inner + 4 && y >= 2 && y < 2 + MENUS[top].1.len() {
        Some(y - 2)
    } else {
        None
    }
}

/// Which menu title (if any) is at column x on the top border.
fn menu_at(x: usize) -> Option<usize> {
    (0..MENUS.len()).find(|&i| x >= menu_x(i) && x < menu_x(i) + MENUS[i].0.len() + 2)
}

fn draw_dropdown<W: Write>(out: &mut W, w: usize, top: usize, sel: usize) -> io::Result<()> {
    let items = MENUS[top].1;
    let (x, lw, sw, inner) = dropdown_geom(w, top);
    let bw = inner + 4;
    put(out, x, 1, &format!("╭{}╮", "─".repeat(bw - 2)), BORDER)?;
    for (i, it) in items.iter().enumerate() {
        let y = 2 + i;
        if matches!(it.2, Cmd::Sep) {
            put(out, x, y, &format!("├{}┤", "─".repeat(bw - 2)), BORDER)?;
        } else {
            put(out, x, y, "│", BORDER)?;
            let left = format!(" {}   ", fit(it.0, lw));
            let right = format!("{} ", fit(it.1, sw));
            if i == sel {
                put(out, x + 1, y, &format!("{}{}", left, right), SEL)?;
            } else {
                put(out, x + 1, y, &left, TXT)?;
                put(out, x + 1 + left.chars().count(), y, &right, DIM)?;
            }
            put(out, x + bw - 1, y, "│", BORDER)?;
        }
    }
    put(out, x, 2 + items.len(), &format!("╰{}╯", "─".repeat(bw - 2)), BORDER)
}

/// (x, y, box width, list height) of the file browser dialog.
/// Rows: border, "look in", list (lh rows), file name, hints, border.
fn browser_geom(w: usize, h: usize) -> (usize, usize, usize, usize) {
    let bw = 62.min(w.saturating_sub(4));
    let lh = h.saturating_sub(9).clamp(2, 15);
    let bh = lh + 5;
    ((w - bw) / 2, h.saturating_sub(bh) / 2, bw, lh)
}

/// Last `n` chars of `s`.
fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

/// Draws the browser; returns where the text cursor should go.
fn draw_browser<W: Write>(out: &mut W, w: usize, h: usize, b: &Browser) -> io::Result<(usize, usize)> {
    let (x, y, bw, lh) = browser_geom(w, h);
    let title = match b.kind {
        BrowseKind::Open => "Open",
        BrowseKind::SaveAs(_) => "Save As",
    };
    draw_frame(out, x, y, bw, lh + 5, title)?;
    put(out, x + 1, y + 1, " Look in: ", DIM)?;
    let dir = tail(&b.dir.display().to_string(), bw - 13);
    put(out, x + 11, y + 1, &fit(&dir, bw - 13), BOLD)?;
    for r in 0..lh {
        let yy = y + 2 + r;
        let idx = b.scroll + r;
        let (text, st) = match b.entries.get(idx) {
            Some((n, is_dir)) => {
                let label = format!(" {}{}", n, if *is_dir && n != ".." { "/" } else { "" });
                let st = if idx == b.sel {
                    SEL
                } else if *is_dir {
                    DIR
                } else {
                    TXT
                };
                (label, st)
            }
            None => (String::new(), TXT),
        };
        put(out, x + 1, yy, &fit(&text, bw - 2), st)?;
    }
    let ny = y + 2 + lh;
    let shown = tail(&b.name, bw - 15);
    put(out, x + 1, ny, " File name: ", DIM)?;
    put(out, x + 13, ny, &fit(&shown, bw - 14), BOLD)?;
    put(out, x + 1, ny + 1, &fit(" Enter OK   Esc Cancel   F4 hidden files", bw - 2), DIM)?;
    Ok((x + 13 + shown.chars().count(), ny))
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn expand(s: &str) -> PathBuf {
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(s[1..].trim_start_matches('/'));
        }
    }
    PathBuf::from(s)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in data.chunks(3) {
        let b1 = ch.get(1).copied().unwrap_or(0) as u32;
        let b2 = ch.get(2).copied().unwrap_or(0) as u32;
        let n = ((ch[0] as u32) << 16) | (b1 << 8) | b2;
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if ch.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        s.push(if ch.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

// ---------------------------------------------------------------- main ---

fn restore_terminal() {
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

fn main() -> io::Result<()> {
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h") | Some("--help")) {
        println!("antpad - tiny terminal notepad\n\nUsage: antpad [FILE]\n\nPress F10 or Alt+F/E/S/H for menus.");
        return Ok(());
    }

    let (w, h) = terminal::size()?;
    let mut ed = Editor::new(w, h);
    if let Some(a) = arg {
        ed.open_path(expand(&a));
    }

    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let res = event_loop(&mut ed);
    restore_terminal();
    res
}

fn event_loop(ed: &mut Editor) -> io::Result<()> {
    let mut out = BufWriter::new(io::stdout());
    loop {
        ed.draw(&mut out)?;
        if ed.quit {
            return Ok(());
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => ed.on_key(k),
            Event::Paste(s) => ed.on_paste(s),
            Event::Mouse(m) => ed.on_mouse(m),
            Event::Resize(w, h) => {
                ed.w = w as usize;
                ed.h = h as usize;
            }
            _ => {}
        }
    }
}
