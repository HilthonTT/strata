//! Tabs: independent sets of panels.
//!
//! The current tab lives in `App::panels` / `App::active` so the rest of the
//! app never needs to know about tabs; other tabs are parked in `App::tabs`.

use strata_plugin::Level;

use super::panel::Panel;
use super::App;

#[derive(Default)]
pub struct Tab {
    pub panels: Vec<Panel>,
    pub active: usize,
}

impl App {
    /// Short titles for every tab (the focused directory's name).
    pub fn tab_titles(&self) -> Vec<String> {
        (0..self.tabs.len())
            .map(|i| {
                let (panels, active) = if i == self.tab {
                    (&self.panels, self.active)
                } else {
                    (&self.tabs[i].panels, self.tabs[i].active)
                };
                panels
                    .get(active)
                    .map(|p| p.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into()))
                    .unwrap_or_default()
            })
            .collect()
    }

    pub(super) fn new_tab(&mut self) {
        if self.tabs.len() >= 9 {
            return self.notify("at most 9 tabs", Level::Warn);
        }
        let p = self.panel();
        let count = self.config.general.panels.clamp(1, 6);
        let panels = (0..count).map(|_| Panel::new(p.vfs.clone(), p.cwd.clone(), p.sort, p.show_hidden)).collect();
        self.tabs.insert(self.tab + 1, Tab { panels, active: 0 });
        self.switch_tab(self.tab + 1, true);
    }

    pub(super) fn close_tab(&mut self) {
        if self.tabs.len() == 1 {
            return self.notify("cannot close the last tab", Level::Warn);
        }
        self.tabs.remove(self.tab);
        let next = self.tab.min(self.tabs.len() - 1);
        let tab = std::mem::take(&mut self.tabs[next]);
        self.panels = tab.panels;
        self.active = tab.active;
        self.tab = next;
        self.after_tab_switch();
    }

    pub(super) fn cycle_tab(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        self.switch_tab((self.tab as isize + delta).rem_euclid(n) as usize, false);
    }

    /// Switches to tab `index` (0-based). `new` is set when the slot already
    /// holds a fresh tab inserted after the current one.
    pub(super) fn switch_tab(&mut self, index: usize, new: bool) {
        if index >= self.tabs.len() || (index == self.tab && !new) {
            return;
        }
        let current = Tab { panels: std::mem::take(&mut self.panels), active: self.active };
        self.tabs[self.tab] = current;
        let next = std::mem::take(&mut self.tabs[index]);
        self.panels = next.panels;
        self.active = next.active;
        self.tab = index;
        self.after_tab_switch();
    }

    fn after_tab_switch(&mut self) {
        self.panels.iter_mut().for_each(Panel::reload);
        self.invalidate_preview();
    }
}
