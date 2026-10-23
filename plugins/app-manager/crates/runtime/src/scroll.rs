//! Scrolling a view's scroll containers (`overflow-y-auto`) by touch.
//!
//! A drag moves the container under the finger with it, with resistance past
//! either end. On release the content carries on at the finger's speed and
//! slows down, and content past an end springs back. The constants follow
//! PocketJS's own kinetic scroller, stepped once per display frame. While
//! the content moves, and for a moment after, the container shows its scroll
//! indicator.

use microts::pocketjs_core::Ui as CoreUi;

/// Display frames per second the motion is stepped at.
const RATE: f32 = 60.0;
/// How much of a drag past either end the content follows.
const RUBBER: f32 = 0.55;
/// How much speed a fling keeps from one frame to the next.
const DECAY: f32 = 0.9672;
/// The spring that pulls content past an end back to it, per unit of mass:
/// stiffness and damping (critically damped).
const SPRING_K: f32 = 170.0;
const SPRING_C: f32 = 26.0;
/// Speed, in px per second, below which the content rests.
const REST_SPEED: f32 = 6.0;
/// Distance, in px, from an end below which a spring rests on it.
const REST_DISTANCE: f32 = 0.5;
/// How much a new drag sample counts in the finger's speed.
const SPEED_WEIGHT: f32 = 0.6;
/// Frames the indicator stays after the content rests, then fades over.
const BAR_HOLD: u32 = 40;
const BAR_FADE: u32 = 12;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Motion {
    /// A finger holds the content.
    Dragging,
    /// The content moves on its own.
    Flinging,
    /// The content rests; the indicator holds or fades over these frames.
    Resting(u32),
}

/// The container being scrolled and how it moves.
#[derive(Debug)]
pub(crate) struct Scroller {
    target: i32,
    offset: f32,
    /// px per second, in the direction the offset grows.
    velocity: f32,
    motion: Motion,
}

impl Scroller {
    /// Takes hold of the container that a drag at (`x`, `y`) moves, if any.
    pub(crate) fn grab(core: &mut CoreUi, x: f32, y: f32) -> Option<Self> {
        let target = core.scroll_target_at(x, y)?;
        let scroller = Self {
            target,
            offset: core.scroll_offset(target),
            velocity: 0.0,
            motion: Motion::Dragging,
        };
        core.set_scroll_bar(target, 1.0);
        Some(scroller)
    }

    /// The container this scroller moves.
    pub(crate) const fn target(&self) -> i32 {
        self.target
    }

    /// Takes hold again of content that is moving or resting.
    pub(crate) fn hold(&mut self, core: &mut CoreUi) {
        self.motion = Motion::Dragging;
        self.velocity = 0.0;
        core.set_scroll_bar(self.target, 1.0);
    }

    /// The finger moved `dy` px down since the last frame, 0 when it held
    /// still: the content moves with it, by less past either end.
    pub(crate) fn drag(&mut self, core: &mut CoreUi, dy: f32) {
        if self.motion != Motion::Dragging {
            return;
        }
        let (top, bottom) = core.scroll_bounds(self.target);
        let mut step = -dy;
        let past = self.offset < top || self.offset > bottom;
        if past || self.offset + step < top || self.offset + step > bottom {
            step *= RUBBER;
        }
        self.offset += step;
        self.velocity = SPEED_WEIGHT * step * RATE + (1.0 - SPEED_WEIGHT) * self.velocity;
        core.set_scroll(self.target, self.offset);
    }

    /// The finger lifted: the content carries on at its speed.
    pub(crate) fn release(&mut self) {
        if self.motion == Motion::Dragging {
            self.motion = Motion::Flinging;
        }
    }

    /// Steps one frame; returns whether the scroller still has anything to
    /// show (motion or the indicator).
    pub(crate) fn step(&mut self, core: &mut CoreUi) -> bool {
        // A relayout clamps the offset; a container with nothing left to
        // scroll, or gone, ends the scroll.
        let (top, bottom) = core.scroll_bounds(self.target);
        if top >= bottom {
            core.set_scroll_bar(self.target, 0.0);
            return false;
        }
        match self.motion {
            Motion::Dragging => true,
            Motion::Flinging => {
                let dt = 1.0 / RATE;
                let end = if self.offset < top {
                    Some(top)
                } else if self.offset > bottom {
                    Some(bottom)
                } else {
                    None
                };
                match end {
                    Some(end) => {
                        let accel = -SPRING_K * (self.offset - end) - SPRING_C * self.velocity;
                        self.velocity += accel * dt;
                        self.offset += self.velocity * dt;
                        let distance = self.offset - end;
                        if distance.abs() < REST_DISTANCE && self.velocity.abs() < REST_SPEED * 10.0
                        {
                            self.offset = end;
                            self.rest();
                        }
                    }
                    None => {
                        self.offset += self.velocity * dt;
                        self.velocity *= DECAY;
                        if self.velocity.abs() < REST_SPEED {
                            self.offset = self.offset.clamp(top, bottom);
                            self.rest();
                        }
                    }
                }
                core.set_scroll(self.target, self.offset);
                true
            }
            Motion::Resting(frames) => {
                self.offset = core.scroll_offset(self.target);
                let frames = frames + 1;
                self.motion = Motion::Resting(frames);
                let shown = if frames <= BAR_HOLD {
                    1.0
                } else {
                    1.0 - (frames - BAR_HOLD) as f32 / BAR_FADE as f32
                };
                core.set_scroll_bar(self.target, shown.max(0.0));
                frames < BAR_HOLD + BAR_FADE
            }
        }
    }

    fn rest(&mut self) {
        self.velocity = 0.0;
        self.motion = Motion::Resting(0);
    }
}
