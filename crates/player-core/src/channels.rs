//! Turning a track's channel layout into the output's.
//!
//! Small enough to have lived inside `playback.rs`, and it did until M3 needed
//! it on wasm32 too. Both shells now hand audio to their ring already in the
//! output's shape (see `playback.rs`'s module docs for why the ring's frame
//! width has to stop depending on the track), so both need this.

/// How wide a frame is either side of the adaptation.
pub struct ChannelShape {
    pub src_channels: u16,
    pub out_channels: u16,
}

/// Rewrites `src_channels`-interleaved frames as `out_channels`-interleaved
/// ones, appending to `dst`.
///
/// Source channels are reused cyclically: mono to stereo duplicates the one
/// channel, and a surround track on a stereo device keeps its first pair.
/// Crude, and deliberately so -- a real downmix has coefficients and a
/// channel map behind it, which is its own job and not one the queue needs.
///
/// Any trailing partial frame is dropped. The resampler emits whole frames,
/// so in practice there is never one.
pub fn adapt_channels(src: &[f32], shape: &ChannelShape, dst: &mut Vec<f32>) {
    let src_channels = shape.src_channels.max(1) as usize;
    let out_channels = shape.out_channels.max(1) as usize;
    if src_channels == out_channels {
        dst.extend_from_slice(src);
        return;
    }
    let frames = src.len() / src_channels;
    dst.reserve(frames * out_channels);
    for frame in 0..frames {
        let base = frame * src_channels;
        for ch in 0..out_channels {
            dst.push(src[base + ch % src_channels]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapt(src: &[f32], src_channels: u16, out_channels: u16) -> Vec<f32> {
        let mut dst = Vec::new();
        adapt_channels(
            src,
            &ChannelShape {
                src_channels,
                out_channels,
            },
            &mut dst,
        );
        dst
    }

    #[test]
    fn matching_layouts_pass_through_untouched() {
        let src = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(adapt(&src, 2, 2), src);
    }

    #[test]
    fn mono_is_duplicated_across_the_outputs() {
        assert_eq!(adapt(&[1.0, 2.0], 1, 2), [1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn extra_source_channels_are_dropped() {
        // 5.1-ish frame down to a stereo device: the first pair survives.
        let src = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(adapt(&src, 6, 2), [1.0, 2.0]);
    }

    #[test]
    fn stereo_up_to_more_outputs_cycles_the_pair() {
        assert_eq!(adapt(&[1.0, 2.0], 2, 4), [1.0, 2.0, 1.0, 2.0]);
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped_rather_than_misaligned() {
        // Half a frame at the end would shift every channel after it, so it
        // goes. The resampler emits whole frames, so this is belt and braces.
        assert_eq!(adapt(&[1.0, 2.0, 3.0], 2, 1), [1.0]);
    }

    #[test]
    fn appending_twice_keeps_the_frames_in_order() {
        // The decoder reuses one buffer across blocks, so adaptation has to
        // append rather than replace.
        let mut dst = Vec::new();
        let shape = ChannelShape {
            src_channels: 1,
            out_channels: 2,
        };
        adapt_channels(&[1.0], &shape, &mut dst);
        adapt_channels(&[2.0], &shape, &mut dst);
        assert_eq!(dst, [1.0, 1.0, 2.0, 2.0]);
    }
}
