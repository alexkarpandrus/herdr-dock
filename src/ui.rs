use crate::Result;
use crate::model::{DockOverview, RepositoryPlan};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    io::{self, Stdout, Write},
    path::Path,
};

pub(crate) struct Ui {
    pub(crate) stdout: Stdout,
}

pub(crate) fn terminal_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[derive(Clone)]
pub(crate) enum Segment {
    Plain(String),
    Styled(Color, String),
}

pub(crate) fn plain(text: impl Into<String>) -> Segment {
    Segment::Plain(text.into())
}

pub(crate) fn styled(color: Color, text: impl Into<String>) -> Segment {
    Segment::Styled(color, text.into())
}

pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) cursor: usize,
}

pub(crate) enum LineAction {
    Edited,
    Submit,
    Cancel,
    Ignored,
}

impl Line {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    pub(crate) fn with_text(initial: &str) -> Self {
        Self {
            cursor: initial.chars().count(),
            text: initial.to_string(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub(crate) fn byte_at(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map(|(index, _)| index)
            .unwrap_or(self.text.len())
    }

    pub(crate) fn char_before(&self) -> Option<char> {
        if self.cursor == 0 {
            None
        } else {
            self.text.chars().nth(self.cursor - 1)
        }
    }

    pub(crate) fn display(&self) -> String {
        let mut out = String::with_capacity(self.text.len() + 1);
        for (index, character) in self.text.chars().enumerate() {
            if index == self.cursor {
                out.push('▏');
            }
            out.push(character);
        }
        if self.cursor >= self.text.chars().count() {
            out.push('▏');
        }
        out
    }

    pub(crate) fn insert(&mut self, character: char) {
        let byte = self.byte_at(self.cursor);
        self.text.insert(byte, character);
        self.cursor += 1;
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte = self.byte_at(self.cursor - 1);
        self.text.remove(byte);
        self.cursor -= 1;
    }

    pub(crate) fn delete_forward(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let byte = self.byte_at(self.cursor);
        self.text.remove(byte);
    }

    pub(crate) fn handle(&mut self, key: &KeyEvent) -> LineAction {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => LineAction::Cancel,
            KeyCode::Enter => LineAction::Submit,
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                LineAction::Edited
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.text.chars().count());
                LineAction::Edited
            }
            KeyCode::Home => {
                self.cursor = 0;
                LineAction::Edited
            }
            KeyCode::End => {
                self.cursor = self.text.chars().count();
                LineAction::Edited
            }
            KeyCode::Backspace => {
                self.backspace();
                LineAction::Edited
            }
            KeyCode::Delete => {
                self.delete_forward();
                LineAction::Edited
            }
            KeyCode::Char('a') if control => {
                self.cursor = 0;
                LineAction::Edited
            }
            KeyCode::Char('e') if control => {
                self.cursor = self.text.chars().count();
                LineAction::Edited
            }
            KeyCode::Char('b') if control => {
                self.cursor = self.cursor.saturating_sub(1);
                LineAction::Edited
            }
            KeyCode::Char('f') if control => {
                self.cursor = (self.cursor + 1).min(self.text.chars().count());
                LineAction::Edited
            }
            KeyCode::Char('u') if control => {
                let byte = self.byte_at(self.cursor);
                self.text.drain(..byte);
                self.cursor = 0;
                LineAction::Edited
            }
            KeyCode::Char('w') if control => {
                while self
                    .char_before()
                    .is_some_and(|character| !character.is_whitespace())
                {
                    self.backspace();
                }
                while self
                    .char_before()
                    .is_some_and(|character| character.is_whitespace())
                {
                    self.backspace();
                }
                LineAction::Edited
            }
            KeyCode::Char(character) => {
                self.insert(character);
                LineAction::Edited
            }
            _ => LineAction::Ignored,
        }
    }
}

pub(crate) enum BaseRefChoice {
    One(String),
    All(String),
}

impl Ui {
    pub(crate) fn start() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { stdout })
    }

    pub(crate) fn frame(&mut self, title: &str, lines: &[String]) -> Result<()> {
        queue!(
            self.stdout,
            MoveTo(0, 0),
            Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print(terminal_text(title)),
            SetAttribute(Attribute::Reset),
            Print("\r\n\r\n")
        )?;
        for line in lines {
            queue!(self.stdout, Print(terminal_text(line)), Print("\r\n"))?;
        }
        self.stdout.flush()?;
        Ok(())
    }

    pub(crate) fn frame_styled(&mut self, title: &str, lines: &[Vec<Segment>]) -> Result<()> {
        queue!(
            self.stdout,
            MoveTo(0, 0),
            Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print(terminal_text(title)),
            SetAttribute(Attribute::Reset),
            Print("\r\n\r\n")
        )?;
        for line in lines {
            for segment in line {
                match segment {
                    Segment::Plain(text) => queue!(self.stdout, Print(terminal_text(text)))?,
                    Segment::Styled(color, text) => queue!(
                        self.stdout,
                        SetForegroundColor(*color),
                        Print(terminal_text(text)),
                        SetForegroundColor(Color::Reset)
                    )?,
                }
            }
            queue!(self.stdout, Print("\r\n"))?;
        }
        self.stdout.flush()?;
        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
    }
}
pub(crate) fn confirm_archive(ui: &mut Ui, dock: &DockOverview) -> Result<bool> {
    let mut line = Line::new();
    loop {
        ui.frame(
            "Archive/remove dock",
            &[
                format!("Dock: {}", dock.name),
                format!("Worktrees: {}", dock.repositories.len()),
                String::new(),
                "This closes the workspace and removes clean worktrees.".into(),
                "Branches and the archived history record remain.".into(),
                String::new(),
                format!("> {}", line.display()),
                String::new(),
                "Type `yes` to archive/remove · Esc cancel".into(),
            ],
        )?;
        match line.handle(&read_key()?) {
            LineAction::Submit => return Ok(line.text.trim().eq_ignore_ascii_case("yes")),
            LineAction::Cancel => return Ok(false),
            _ => {}
        }
    }
}
pub(crate) fn confirm_complete(ui: &mut Ui, dock: &DockOverview) -> Result<bool> {
    ui.frame(
        "Close dock",
        &[
            format!("Dock: {}", dock.name),
            String::new(),
            "This closes its workspace, tabs, and running processes.".into(),
            "Worktrees and resumable agent session IDs remain.".into(),
            String::new(),
            "Y close/done · any other key cancel".into(),
        ],
    )?;
    Ok(matches!(
        read_key()?.code,
        KeyCode::Char('y') | KeyCode::Char('Y')
    ))
}
pub(crate) fn confirm_create(
    ui: &mut Ui,
    name: &str,
    branch: &str,
    root: &Path,
    plans: &[RepositoryPlan],
) -> Result<bool> {
    let mut lines = vec![
        format!("Name:   {name}"),
        format!("Branch: {branch}"),
        format!("Root:   {}", root.display()),
        String::new(),
        "Repositories:".into(),
    ];
    for plan in plans {
        lines.push(format!("  {}  <-  {}", plan.repository.name, plan.base_ref));
    }
    lines.extend([String::new(), "Y create · any other key cancel".into()]);
    ui.frame("Create dock", &lines)?;
    Ok(matches!(
        read_key()?.code,
        KeyCode::Char('y') | KeyCode::Char('Y')
    ))
}
pub(crate) fn show_notice(ui: &mut Ui, title: &str, text: &str) -> Result<()> {
    let mut lines = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
    lines.push(String::new());
    lines.push("Press any key".into());
    ui.frame(title, &lines)?;
    read_key()?;
    Ok(())
}
pub(crate) fn read_key() -> Result<KeyEvent> {
    loop {
        if let Event::Key(key) = event::read()?
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
            return Ok(key);
        }
    }
}
pub(crate) fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for character in value.trim().chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_end_matches('_').into()
}
