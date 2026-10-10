//! Padlock icons rendered in code so the unlocked state is red in every icon theme.

const SIZES: [i32; 5] = [16, 22, 24, 32, 48];
const SUPERSAMPLE: i32 = 4;

const RED: Rgba = Rgba(0xd3, 0x2f, 0x2f, 0xff);
const WHITE: Rgba = Rgba(0xff, 0xff, 0xff, 0xff);
const GREY: Rgba = Rgba(0xb0, 0xbe, 0xc5, 0xff);
const CLEAR: Rgba = Rgba(0, 0, 0, 0);

#[derive(Clone, Copy)]
struct Rgba(u8, u8, u8, u8);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    Locked,
    Unlocked,
}

pub fn icon(state: LockState) -> Vec<ksni::Icon> {
    SIZES.iter().map(|&size| render(state, size)).collect()
}

fn render(state: LockState, size: i32) -> ksni::Icon {
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for py in 0..size {
        for px in 0..size {
            // Average premultiplied colour over a grid of subpixel samples.
            let mut sum = [0f32; 4];
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let x = (px as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                    let Rgba(r, g, b, a) = colour_at(state, x, y);
                    let alpha = a as f32 / 255.0;
                    sum[0] += alpha;
                    sum[1] += r as f32 * alpha;
                    sum[2] += g as f32 * alpha;
                    sum[3] += b as f32 * alpha;
                }
            }
            let alpha = sum[0] / samples;
            let channel = |total: f32| {
                if alpha == 0.0 {
                    0
                } else {
                    (total / samples / alpha).round() as u8
                }
            };
            data.extend_from_slice(&[
                (alpha * 255.0).round() as u8,
                channel(sum[1]),
                channel(sum[2]),
                channel(sum[3]),
            ]);
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}

/// Colour at a point in the unit square.
fn colour_at(state: LockState, x: f32, y: f32) -> Rgba {
    let (background, lock, keyhole) = match state {
        LockState::Unlocked => (RED, WHITE, RED),
        LockState::Locked => (CLEAR, GREY, CLEAR),
    };
    if in_keyhole(x, y) && in_body(x, y) {
        return keyhole;
    }
    if in_body(x, y) || in_shackle(state, x, y) {
        return lock;
    }
    if state == LockState::Unlocked && in_rounded_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.22) {
        return background;
    }
    CLEAR
}

fn in_body(x: f32, y: f32) -> bool {
    in_rounded_rect(x, y, 0.22, 0.46, 0.78, 0.84, 0.06)
}

fn in_keyhole(x: f32, y: f32) -> bool {
    let circle = (x - 0.5).powi(2) + (y - 0.6).powi(2) <= 0.055f32.powi(2);
    let slot = (0.475..=0.525).contains(&x) && (0.6..=0.73).contains(&y);
    circle || slot
}

fn in_shackle(state: LockState, x: f32, y: f32) -> bool {
    // An open shackle is raised, with its right leg out of the body.
    let (lift, right_leg_end) = match state {
        LockState::Locked => (0.0, 0.46),
        LockState::Unlocked => (0.12, 0.30),
    };
    let centre_y = 0.36 - lift;
    let (outer, inner) = (0.2, 0.12);
    let distance = ((x - 0.5).powi(2) + (y - centre_y).powi(2)).sqrt();
    let arch = y <= centre_y && (inner..=outer).contains(&distance);
    let left_leg = (0.3..=0.38).contains(&x) && (centre_y..=0.46).contains(&y);
    let right_leg = (0.62..=0.7).contains(&x) && (centre_y..=right_leg_end).contains(&y);
    arch || left_leg || right_leg
}

fn in_rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + radius, x1 - radius);
    let cy = y.clamp(y0 + radius, y1 - radius);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius.powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(icon: &ksni::Icon, x: i32, y: i32) -> [u8; 4] {
        let offset = ((y * icon.width + x) * 4) as usize;
        icon.data[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn every_size_has_full_argb_data() {
        for state in [LockState::Locked, LockState::Unlocked] {
            for icon in icon(state) {
                assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
            }
        }
    }

    #[test]
    fn unlocked_is_red_and_locked_has_no_background() {
        let unlocked = render(LockState::Unlocked, 32);
        let locked = render(LockState::Locked, 32);
        // A point inside the badge but outside the padlock.
        assert_eq!(pixel(&unlocked, 3, 16), [0xff, 0xd3, 0x2f, 0x2f]);
        assert_eq!(pixel(&locked, 3, 16)[0], 0);
    }
}
