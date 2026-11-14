// SPDX-License-Identifier: GPL-3.0-only
//! Tabs and splits: a tab owns a grid of terminals (split panes).

use std::sync::Mutex;

use cosmic::widget::{self, pane_grid, segmented_button};

use crate::terminal::{Terminal, TerminalId};

/// One terminal inside a split.
pub struct TermPane {
    pub id: TerminalId,
    pub terminal: Mutex<Terminal>,
    /// Widget id used to give this terminal the keyboard focus.
    pub widget_id: widget::Id,
}

/// One tab: a tree of split panes plus the pane that has the focus.
pub struct Tab {
    /// Entity of this tab in the tab bar's model.
    pub entity: segmented_button::Entity,
    pub panes: pane_grid::State<TermPane>,
    pub focus: pane_grid::Pane,
}

impl Tab {
    /// A tab that starts with a single terminal.
    pub fn new(entity: segmented_button::Entity, first: TermPane) -> Self {
        let (panes, focus) = pane_grid::State::new(first);
        Self {
            entity,
            panes,
            focus,
        }
    }

    pub fn focused(&self) -> Option<&TermPane> {
        self.panes.get(self.focus)
    }

    /// The pane that holds terminal `id`, if it is in this tab.
    pub fn pane_of(&self, id: TerminalId) -> Option<pane_grid::Pane> {
        self.panes
            .iter()
            .find(|(_, pane)| pane.id == id)
            .map(|(handle, _)| *handle)
    }

    /// Tell every terminal of this tab whether it is the focused one (cursor style).
    pub fn sync_focus(&self) {
        for (handle, pane) in self.panes.iter() {
            pane.terminal
                .lock()
                .unwrap()
                .set_focused(*handle == self.focus);
        }
    }

    /// Mark every terminal of this tab as not focused (tab is in the background).
    pub fn unfocus_all(&self) {
        for (_, pane) in self.panes.iter() {
            pane.terminal.lock().unwrap().set_focused(false);
        }
    }
}
