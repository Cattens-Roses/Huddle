//! Geometry shared by photo and video viewers. 100% means fitted to the stage.
use gpui::{Bounds, Pixels, Point, point, px};
use std::{cell::Cell, rc::Rc};

pub(super) const MAX: f32 = 5.;

#[derive(Clone, Copy, Default)]
pub(super) struct Geometry {
    pub bounds: Bounds<Pixels>,
    pub aspect: f32,
}

pub(super) struct Zoom {
    pub factor: f32,
    pub geometry: Rc<Cell<Geometry>>,
    pan: Point<Pixels>,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            factor: 1.,
            geometry: Rc::default(),
            pan: Point::default(),
            drag: None,
        }
    }
}

impl Zoom {
    pub fn reset(&mut self) {
        self.factor = 1.;
        self.pan = Point::default();
        self.drag = None;
    }

    pub fn center(&self) -> Point<Pixels> {
        self.geometry.get().bounds.center()
    }

    pub fn at(&mut self, factor: f32, pointer: Point<Pixels>) {
        if !factor.is_finite() {
            return;
        }
        let factor = factor.clamp(1., MAX);
        let anchor = pointer - self.center();
        self.pan = anchor + (self.offset() - anchor) * (factor / self.factor);
        self.factor = factor;
        self.pan = self.offset();
        self.drag = None;
    }

    pub fn offset(&self) -> Point<Pixels> {
        let Geometry { bounds, aspect } = self.geometry.get();
        let width = bounds.size.width.as_f32().max(0.);
        let height = bounds.size.height.as_f32().max(0.);
        let (fitted_width, fitted_height) = if aspect.is_finite() && aspect > 0. {
            let fitted_width = width.min(height * aspect);
            (fitted_width, fitted_width / aspect)
        } else {
            (width, height)
        };
        let max_x = ((fitted_width * self.factor - width) / 2.).max(0.);
        let max_y = ((fitted_height * self.factor - height) / 2.).max(0.);
        point(
            px(self.pan.x.as_f32().clamp(-max_x, max_x)),
            px(self.pan.y.as_f32().clamp(-max_y, max_y)),
        )
    }

    pub fn begin_drag(&mut self, pointer: Point<Pixels>) {
        if self.factor > 1. {
            self.drag = Some((pointer, self.offset()));
        }
    }

    pub fn drag_to(&mut self, pointer: Point<Pixels>) -> bool {
        let Some((start, pan)) = self.drag else {
            return false;
        };
        self.pan = pan + pointer - start;
        self.pan = self.offset();
        true
    }

    pub fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;
    fn zoom(aspect: f32) -> Zoom {
        let zoom = Zoom::default();
        zoom.geometry.set(Geometry {
            bounds: Bounds::new(point(px(10.), px(20.)), size(px(400.), px(300.))),
            aspect,
        });
        zoom
    }

    #[test]
    fn wheel_zoom_keeps_the_content_under_the_pointer_and_limits_scale() {
        let mut zoom = zoom(4. / 3.);
        let pointer = zoom.center() + point(px(40.), px(30.));
        zoom.at(2., pointer);
        assert_eq!(zoom.offset(), point(px(-40.), px(-30.)));
        zoom.at(100., zoom.center());
        assert_eq!(zoom.factor, MAX);
        zoom.at(0., zoom.center());
        assert_eq!(zoom.factor, 1.);
        assert_eq!(zoom.offset(), Point::default());
    }

    #[test]
    fn dragging_is_clamped_to_the_image_including_letterboxed_media() {
        let mut zoom = zoom(2.);
        zoom.begin_drag(zoom.center());
        assert!(!zoom.drag_to(point(px(999.), px(999.))));
        zoom.at(2., zoom.center());
        zoom.begin_drag(zoom.center());
        zoom.drag_to(point(px(999.), px(999.)));
        assert_eq!(zoom.offset(), point(px(200.), px(50.)));
        assert!(zoom.end_drag());
        assert!(!zoom.drag_to(Point::default()));
        zoom.reset();
        assert_eq!(zoom.offset(), Point::default());
        assert!(!zoom.dragging());
    }

    #[test]
    fn resizing_and_portrait_media_do_not_allow_panning_into_blank_space() {
        let mut zoom = zoom(0.5);
        zoom.at(2., zoom.center());
        zoom.begin_drag(zoom.center());
        zoom.drag_to(point(px(999.), px(999.)));
        assert_eq!(zoom.offset().x, px(0.));
        assert_eq!(zoom.offset().y, px(150.));
        zoom.geometry.set(Geometry {
            bounds: Bounds::new(Point::default(), size(px(800.), px(800.))),
            aspect: 0.5,
        });
        assert_eq!(zoom.offset().x, px(0.));
        zoom.at(f32::NAN, Point::default());
        assert_eq!(zoom.factor, 2.);
    }
}
