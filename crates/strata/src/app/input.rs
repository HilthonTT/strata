//! Keyboard and mouse input routing.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use strata_config::{Binding, KeyPress, Lookup};
use strata_core::ops::Conflict;

use super::overlay::{Confirm, InputPurpose, Overlay, PickerPurpose};
use super::{App, Focus, View};

impl App {
    pub(super) fn on_key(&mut self, key: KeyEvent) {
        if self.overlay.is_some() {
            self.on_overlay_key(key);
            self.process_plugin_requests();
            return;
        }
        let press = KeyPress::from_event(&key);
        if self.pending_keys.is_empty() && self.on_view_key(press) {
            return;
        }
        self.pending_keys.push(press);
        match self.keymap.lookup(&self.pending_keys) {
            Lookup::Exact(binding) => {
                self.pending_keys.clear();
                self.run_binding(binding);
            }
            Lookup::Prefix(_) => self.pending_since = Instant::now(),
            Lookup::None => {
                let retry = self.pending_keys.len() > 1;
                self.pending_keys.clear();
                // `g x` unbound: treat `x` as a fresh key press.
                if retry {
                    self.on_key(key);
                }
            }
        }
        self.process_plugin_requests();
    }

    /// Fires a bound prefix (e.g. `g`) once the sequence timed out.
    pub(super) fn flush_pending_keys(&mut self) {
        let keys = std::mem::take(&mut self.pending_keys);
        if let Lookup::Prefix(Some(binding)) = self.keymap.lookup(&keys) {
            self.run_binding(binding);
        }
    }

    pub(super) fn run_binding(&mut self, binding: Binding) {
        match binding {
            Binding::Action(action) => self.dispatch(action),
            Binding::Command(line) => self.run_command(&line),
            Binding::Plugin { callback, .. } => self.call_plugin(callback, None),
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.overlay.as_mut() {
            Some(Overlay::Input(input)) => match key.code {
                KeyCode::Esc => self.cancel_input(),
                KeyCode::Enter => self.submit_input(),
                KeyCode::Tab => self.complete_input(),
                KeyCode::Backspace if ctrl => input.delete_word(),
                KeyCode::Char('w') if ctrl => input.delete_word(),
                KeyCode::Char('u') if ctrl => {
                    input.value.clear();
                    input.cursor = 0;
                }
                KeyCode::Char('a') if ctrl => input.home(),
                KeyCode::Char('e') if ctrl => input.end(),
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Left => input.left(),
                KeyCode::Right => input.right(),
                KeyCode::Home => input.home(),
                KeyCode::End => input.end(),
                KeyCode::Char(c) => input.insert(c),
                _ => {}
            },
            Some(Overlay::Picker(picker)) => {
                match key.code {
                    KeyCode::Esc => return self.cancel_picker(),
                    KeyCode::Enter => return self.submit_picker(),
                    KeyCode::Up => picker.move_by(-1),
                    KeyCode::Down | KeyCode::Tab => picker.move_by(1),
                    KeyCode::Char('k') | KeyCode::Char('p') if ctrl => picker.move_by(-1),
                    KeyCode::Char('j') | KeyCode::Char('n') if ctrl => picker.move_by(1),
                    KeyCode::PageUp => picker.move_by(-10),
                    KeyCode::PageDown => picker.move_by(10),
                    KeyCode::Backspace => {
                        picker.query.pop();
                        picker.refilter();
                    }
                    KeyCode::Char(c) => {
                        picker.query.push(c);
                        picker.refilter();
                    }
                    _ => {}
                }
                self.on_picker_moved();
            }
            Some(Overlay::Conflict(_)) => {
                let choice = match key.code {
                    KeyCode::Char('k') | KeyCode::Char('K') | KeyCode::Enter => Some(Conflict::KeepBoth),
                    KeyCode::Char('o') | KeyCode::Char('O') => Some(Conflict::Overwrite),
                    KeyCode::Char('s') | KeyCode::Char('S') => Some(Conflict::Skip),
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => None,
                    _ => return,
                };
                if let Some(Overlay::Conflict(mut state)) = self.overlay.take() {
                    if let Some(conflict) = choice {
                        state.transfer.conflict = conflict;
                        self.start_transfer(state.transfer);
                    }
                }
            }
            Some(Overlay::Confirm(_)) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    if let Some(Overlay::Confirm(c)) = self.overlay.take() {
                        self.confirmed(c.action);
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                _ => {}
            },
            Some(Overlay::Help { scroll }) | Some(Overlay::Text(super::overlay::TextPopup { scroll, .. })) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => self.overlay = None,
                    KeyCode::Char('j') | KeyCode::Down => *scroll += 1,
                    KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Char('d') | KeyCode::PageDown => *scroll += 15,
                    KeyCode::Char('u') | KeyCode::PageUp => *scroll = scroll.saturating_sub(15),
                    KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
                    KeyCode::Char('G') | KeyCode::End => *scroll = usize::MAX / 2,
                    _ => {}
                }
            }
            None => {}
        }
        // Keep the live filter in sync with what is typed.
        if let Some(Overlay::Input(input)) = &self.overlay {
            if matches!(input.purpose, InputPurpose::Filter) && input.value != self.panel().filter {
                let value = input.value.clone();
                self.panel_mut().set_filter(&value);
            }
        }
    }

    fn on_picker_moved(&mut self) {
        // Theme picker previews the theme under the cursor.
        if let Some(Overlay::Picker(p)) = &self.overlay {
            if let PickerPurpose::Theme { .. } = p.purpose {
                if let Some(name) = p.selected().map(str::to_string) {
                    self.apply_theme(&name, false);
                }
            }
        }
    }

    fn cancel_picker(&mut self) {
        match self.overlay.take() {
            Some(Overlay::Picker(p)) => match p.purpose {
                PickerPurpose::Theme { original } => {
                    self.apply_theme(&original, false);
                }
                PickerPurpose::Plugin(callback) => self.call_plugin(callback, Some(None)),
                PickerPurpose::Find { .. } | PickerPurpose::Sort | PickerPurpose::Grep { .. } => {}
            },
            other => self.overlay = other,
        }
    }

    fn cancel_input(&mut self) {
        match self.overlay.take() {
            Some(Overlay::Input(input)) => match input.purpose {
                InputPurpose::Filter => self.panel_mut().set_filter(""),
                InputPurpose::Plugin(callback) => self.call_plugin(callback, Some(None)),
                _ => {}
            },
            other => self.overlay = other,
        }
    }

    pub(super) fn confirmed(&mut self, action: Confirm) {
        match action {
            Confirm::Delete { vfs, paths, permanent } => self.start_delete(vfs, paths, permanent),
            Confirm::DockerRemove { id, name } => {
                super::workers::container_action(self.tx.clone(), id, name, strata_sys::docker::ContainerAction::Remove)
            }
            Confirm::SavePassword { name, password } => match strata_core::secrets::set(&name, &password) {
                Ok(()) => {
                    self.nas.saved_passwords.insert(name.clone());
                    self.info(format!("saved the password for {name} in the system keychain"));
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            Confirm::Quit => {
                self.jobs.cancel_all();
                self.quit();
            }
        }
    }

    pub(super) fn on_paste(&mut self, text: &str) {
        match self.overlay.as_mut() {
            Some(Overlay::Input(input)) => text.chars().filter(|c| !c.is_control()).for_each(|c| input.insert(c)),
            Some(Overlay::Picker(p)) => {
                p.query.push_str(text.trim());
                p.refilter();
            }
            _ => {}
        }
    }

    pub(super) fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.overlay.is_some() {
            return;
        }
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;

        if self.view == View::Files {
            if let Some(i) = self.layout.panels.iter().position(inside) {
                let rect = self.layout.panels[i];
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.panels[i].move_by(3),
                    MouseEventKind::ScrollUp => self.panels[i].move_by(-3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.active = i;
                        self.focus = Focus::Panels;
                        // Row 0 is the border, row 1 the column header.
                        let row = y.saturating_sub(rect.y + 2) as usize + self.panels[i].offset;
                        if y >= rect.y + 2 && row < self.panels[i].len() {
                            self.panels[i].move_to(row);
                            if self.is_double_click(x, y) {
                                self.dispatch(strata_config::Action::Open);
                            }
                        }
                    }
                    _ => {}
                }
                return;
            }
            if self.layout.preview.is_some_and(|r| inside(&r)) {
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.scroll_preview(3),
                    MouseEventKind::ScrollUp => self.scroll_preview(-3),
                    _ => {}
                }
                return;
            }
            if let Some(rect) = self.layout.sidebar.filter(|r| inside(r)) {
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.sidebar.move_by(1),
                    MouseEventKind::ScrollUp => self.sidebar.move_by(-1),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let row = y.saturating_sub(rect.y + 1) as usize + self.sidebar.offset;
                        if self.sidebar.items.get(row).is_some_and(|i| i.selectable()) {
                            self.sidebar.cursor = row;
                            self.open_sidebar_item();
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        if let Some(rect) = self.layout.list.filter(|r| inside(r)) {
            let delta = match mouse.kind {
                MouseEventKind::ScrollDown => 1,
                MouseEventKind::ScrollUp => -1,
                MouseEventKind::Down(MouseButton::Left) => {
                    let row = y.saturating_sub(rect.y + 2) as usize;
                    self.set_view_cursor(row);
                    if self.is_double_click(x, y) {
                        self.dispatch(strata_config::Action::Open);
                    }
                    0
                }
                _ => 0,
            };
            if delta != 0 {
                self.move_view_cursor(delta);
            }
        }
    }

    fn is_double_click(&mut self, x: u16, y: u16) -> bool {
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(t, lx, ly)| lx == x && ly == y && now.duration_since(t) < Duration::from_millis(400));
        self.last_click = if double { None } else { Some((now, x, y)) };
        double
    }
}
