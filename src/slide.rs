// SPDX-License-Identifier: GPL-3.0-only
//! A wrapper that shifts its content vertically without re-laying it out.
//!
//! Derived from iced's `Float` widget (MIT, © Héctor Ramón Jiménez), reduced to a plain
//! translation and made independent of the theme catalog, because `cosmic::Theme` does
//! not implement `float::Catalog` and cannot get an impl from this crate.
//!
//! While the offset is non-zero the content is drawn and updated through the overlay
//! layer with a transformation, so hit-testing follows the moved content. With an offset
//! of zero it behaves like a transparent wrapper.

use cosmic::iced::core::{
    self, Clipboard, Element, Event, Layout, Length, Rectangle, Shell, Size, Transformation,
    Vector, Widget, layout, mouse, overlay, renderer,
    widget::{self, tree},
};

pub struct SlideIn<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    /// Shift in logical pixels; negative moves the content up.
    offset: f32,
}

impl<'a, Message, Theme, Renderer> SlideIn<'a, Message, Theme, Renderer> {
    pub fn new(content: impl Into<Element<'a, Message, Theme, Renderer>>, offset: f32) -> Self {
        Self {
            content: content.into(),
            offset,
        }
    }

    fn is_shifted(&self) -> bool {
        self.offset != 0.0
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for SlideIn<'_, Message, Theme, Renderer>
where
    Renderer: core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<tree::Tree> {
        self.content.as_widget().children()
    }

    fn diff(&mut self, tree: &mut widget::Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.is_shifted() {
            // Handled through the overlay with the inverse transformation.
            return;
        }
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if self.is_shifted() {
            return;
        }
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.is_shifted() {
            return mouse::Interaction::None;
        }
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn overlay<'a>(
        &'a mut self,
        state: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        offset: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        if !self.is_shifted() {
            return self
                .content
                .as_widget_mut()
                .overlay(state, layout, renderer, viewport, offset);
        }

        let transformation = Transformation::translate(offset.x, offset.y + self.offset);

        Some(overlay::Element::new(Box::new(ShiftedOverlay {
            slide: self,
            state,
            layout,
            viewport: *viewport,
            transformation,
        })))
    }

    fn drag_destinations(
        &self,
        state: &widget::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        dnd_rectangles: &mut core::clipboard::DndDestinationRectangles,
    ) {
        if let Some(l) = layout.children().next() {
            self.content.as_widget().drag_destinations(
                state,
                l.with_virtual_offset(layout.virtual_offset()),
                renderer,
                dnd_rectangles,
            );
        }
    }
}

impl<'a, Message, Theme, Renderer> From<SlideIn<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: core::Renderer + 'a,
{
    fn from(slide: SlideIn<'a, Message, Theme, Renderer>) -> Self {
        Element::new(slide)
    }
}

struct ShiftedOverlay<'a, 'b, Message, Theme, Renderer> {
    slide: &'a mut SlideIn<'b, Message, Theme, Renderer>,
    state: &'a mut widget::Tree,
    layout: Layout<'a>,
    viewport: Rectangle,
    transformation: Transformation,
}

impl<Message, Theme, Renderer> core::Overlay<Message, Theme, Renderer>
    for ShiftedOverlay<'_, '_, Message, Theme, Renderer>
where
    Renderer: core::Renderer,
{
    fn layout(&mut self, _renderer: &Renderer, _bounds: Size) -> layout::Node {
        let bounds = self.layout.bounds() * self.transformation;
        layout::Node::new(bounds.size()).move_to(bounds.position())
    }

    fn update(
        &mut self,
        event: &Event,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let inverse = self.transformation.inverse();
        self.slide.content.as_widget_mut().update(
            self.state,
            event,
            self.layout,
            cursor * inverse,
            renderer,
            clipboard,
            shell,
            &(self.viewport * inverse),
        );
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let inverse = self.transformation.inverse();
        renderer.with_layer(self.viewport, |renderer| {
            renderer.with_transformation(self.transformation, |renderer| {
                self.slide.content.as_widget().draw(
                    self.state,
                    renderer,
                    theme,
                    style,
                    self.layout,
                    cursor * inverse,
                    &(self.viewport * inverse),
                );
            });
        });
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !cursor.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }
        let inverse = self.transformation.inverse();
        self.slide.content.as_widget().mouse_interaction(
            self.state,
            self.layout,
            cursor * inverse,
            &(self.viewport * inverse),
            renderer,
        )
    }

    fn index(&self) -> f32 {
        0.5
    }

    fn overlay<'a>(
        &'a mut self,
        _layout: Layout<'_>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.slide.content.as_widget_mut().overlay(
            self.state,
            self.layout,
            renderer,
            &(self.viewport * self.transformation.inverse()),
            self.transformation.translation(),
        )
    }
}
