// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which codec a picture is in. The media link carries frames without
//! saying (see `rinflow_core::media`): a key frame tells, by the parameter
//! sets it starts with. Both codecs come as NAL units with start codes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
        }
    }
}

/// The first byte of each NAL unit in `frame`, with the byte after it.
fn nal_headers(frame: &[u8]) -> impl Iterator<Item = (u8, u8)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        while at + 3 < frame.len() {
            if frame[at..at + 3] == [0, 0, 1] {
                let header = at + 3;
                at = header + 1;
                return Some((frame[header], frame.get(header + 1).copied().unwrap_or(0)));
            }
            at += 1;
        }
        None
    })
}

/// The codec of a key frame, from its parameter sets: H.265's sequence
/// parameter set is NAL type 33 with layer 0 and temporal ID 1 (the bytes
/// 0x42 0x01), H.264's is NAL type 7. `None`: neither is there.
pub fn detect(key_frame: &[u8]) -> Option<Codec> {
    let mut h264 = false;
    for (first, second) in nal_headers(key_frame) {
        // forbidden_zero_bit
        if first & 0x80 != 0 {
            continue;
        }
        let h265_type = (first >> 1) & 0x3F;
        if matches!(h265_type, 32..=34) && first & 1 == 0 && second == 1 {
            return Some(Codec::H265);
        }
        if first & 0x1F == 7 {
            h264 = true;
        }
    }
    h264.then_some(Codec::H264)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_sets_tell_the_codec() {
        // NanoKVM's H.265 key frame: VPS, SPS, PPS, IDR.
        let h265 = [
            0, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 0, 1, 0x42, 0x01, 0x01, 0, 0, 0, 1, 0x44, 0x01,
            0xC1, 0, 0, 0, 1, 0x26, 0x01, 0xAF,
        ];
        assert_eq!(detect(&h265), Some(Codec::H265));
        // An H.264 key frame with an access unit delimiter: AUD, SPS, PPS, IDR.
        let h264 = [
            0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x67, 0x4D, 0x00, 0x28, 0, 0, 0, 1, 0x68, 0xEE, 0,
            0, 1, 0x65, 0x88,
        ];
        assert_eq!(detect(&h264), Some(Codec::H264));
        // A frame without parameter sets says nothing.
        assert_eq!(detect(&[0, 0, 0, 1, 0x02, 0x01, 0xD0]), None);
        assert_eq!(detect(&[0, 0, 1]), None);
        assert_eq!(detect(&[]), None);
    }
}
