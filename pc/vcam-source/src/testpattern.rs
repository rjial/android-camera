//! NV12 test pattern, shown by the virtual camera until `acam.exe` streams real phone frames into
//! the shared memory (see `vcam-ipc` / the Windows plan).

pub use vcam_ipc::{FRAME_LEN, HEIGHT, WIDTH};

pub const FPS: u32 = 30;

/// Fills an NV12 buffer with a bar sweeping left to right, one sweep every two seconds, so the
/// camera is visibly live without needing real video.
pub fn fill_test_pattern(buf: &mut [u8], frame_index: u64) {
    assert_eq!(buf.len(), FRAME_LEN);
    let (y, uv) = buf.split_at_mut(WIDTH * HEIGHT);
    let period = (FPS as u64) * 2;
    let bar_x = ((frame_index % period) * WIDTH as u64 / period) as usize;
    for row in 0..HEIGHT {
        let line = &mut y[row * WIDTH..(row + 1) * WIDTH];
        for (x, px) in line.iter_mut().enumerate() {
            // Limited-range luma: 16 (black) background, 235 (white) bar.
            *px = if x.abs_diff(bar_x) < WIDTH / 40 { 235 } else { 16 };
        }
    }
    uv.fill(128);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_expected_length_and_neutral_chroma() {
        let mut buf = vec![0u8; FRAME_LEN];
        fill_test_pattern(&mut buf, 0);
        assert!(buf[WIDTH * HEIGHT..].iter().all(|&b| b == 128));
    }

    #[test]
    fn bar_moves_between_frames() {
        let mut a = vec![0u8; FRAME_LEN];
        let mut b = vec![0u8; FRAME_LEN];
        fill_test_pattern(&mut a, 0);
        fill_test_pattern(&mut b, (FPS as u64) / 2);
        assert_ne!(a, b);
    }

    #[test]
    fn sweep_wraps_around_the_period() {
        let mut a = vec![0u8; FRAME_LEN];
        let mut b = vec![0u8; FRAME_LEN];
        fill_test_pattern(&mut a, 0);
        fill_test_pattern(&mut b, (FPS as u64) * 2);
        assert_eq!(a, b);
    }
}
