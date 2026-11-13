// SPDX-License-Identifier: GPL-3.0-only
//! Right-click context menu (derived from cosmic-term's `menu.rs`).

use std::collections::HashMap;

use cosmic::{
    Element,
    iced::{
        Background, Length, Point, advanced::widget::text::Style as TextStyle, core::Border,
    },
    theme,
    widget::{
        self, Column, divider,
        menu::{key_bind::KeyBind, menu_button},
        space,
    },
};

use crate::{Action, Message};

#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub position: Option<Point>,
    pub local_position: Option<Point>,
    pub link: Option<String>,
}

/// Build the context menu. `has_selection` greys out "Copy" when nothing is selected.
pub fn context_menu<'a>(
    key_binds: &HashMap<KeyBind, Action>,
    has_selection: bool,
    link: Option<String>,
) -> Element<'a, Message> {
    let find_key = |action: &Action| -> String {
        key_binds
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(bind, _)| bind.to_string())
            .unwrap_or_default()
    };

    fn key_style(theme: &cosmic::Theme) -> TextStyle {
        let mut color = theme.cosmic().background.component.on;
        color.alpha *= 0.75;
        TextStyle {
            color: Some(color.into()),
        }
    }

    let item = |label: &'static str, key: String, message: Option<Message>| {
        menu_button(vec![
            widget::text(label).into(),
            space::horizontal().into(),
            widget::text(key)
                .class(theme::Text::Custom(key_style))
                .into(),
        ])
        .on_press_maybe(message)
    };

    let mut rows: Vec<Element<'a, Message>> = Vec::new();

    if link.is_some() {
        rows.push(item("Open link", String::new(), Some(Message::OpenLink)).into());
        rows.push(item("Copy link", String::new(), Some(Message::CopyLink)).into());
        rows.push(divider::horizontal::light().into());
    }

    rows.push(
        item(
            "Copy",
            find_key(&Action::Copy),
            has_selection.then_some(Message::MenuAction(Action::Copy)),
        )
        .into(),
    );
    rows.push(
        item(
            "Paste",
            find_key(&Action::Paste),
            Some(Message::MenuAction(Action::Paste)),
        )
        .into(),
    );
    rows.push(
        item(
            "Select all",
            find_key(&Action::SelectAll),
            Some(Message::MenuAction(Action::SelectAll)),
        )
        .into(),
    );

    widget::container(Column::with_children(rows))
        .padding(1)
        .style(|theme| {
            let cosmic = theme.cosmic();
            let component = &cosmic.background.component;
            widget::container::Style {
                icon_color: Some(component.on.into()),
                text_color: Some(component.on.into()),
                background: Some(Background::Color(component.base.into())),
                border: Border {
                    radius: cosmic.radius_s().map(|x| x + 1.0).into(),
                    width: 1.0,
                    color: component.divider.into(),
                },
                ..Default::default()
            }
        })
        .width(Length::Fixed(260.0))
        .into()
}
