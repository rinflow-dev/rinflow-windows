// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Where the picture goes in the window: as large as fits, in the middle.
//!
//! Also [`Screen`], the size of a peer's desktop in its points.

/// A rectangle in pixels of the window's client area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// The picture (`content`, pixels) as large as it fits in the client area
/// (`area`) without changing its shape, in the middle; bars fill the rest.
pub fn fit(content: (u32, u32), area: (u32, u32)) -> Rect {
    let (cw, ch) = (f64::from(content.0.max(1)), f64::from(content.1.max(1)));
    let (aw, ah) = (f64::from(area.0), f64::from(area.1));
    let scale = (aw / cw).min(ah / ch);
    let (width, height) = ((cw * scale).round(), (ch * scale).round());
    let left = ((aw - width) / 2.0).floor();
    let top = ((ah - height) / 2.0).floor();
    Rect {
        left: left as i32,
        top: top as i32,
        right: (left + width) as i32,
        bottom: (top + height) as i32,
    }
}

/// The peer's screen, in its logical points (as its displays describe it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_fits_with_bars_on_the_longer_side() {
        // A 16:9 picture in a 4:3 window: bars above and below.
        assert_eq!(
            fit((1920, 1080), (800, 600)),
            Rect {
                left: 0,
                top: 75,
                right: 800,
                bottom: 525
            }
        );
        // In a wide window: bars left and right.
        assert_eq!(
            fit((1920, 1080), (2000, 900)),
            Rect {
                left: 200,
                top: 0,
                right: 1800,
                bottom: 900
            }
        );
        // Exactly its size.
        assert_eq!(
            fit((1920, 1080), (1920, 1080)),
            Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080
            }
        );
    }
}
