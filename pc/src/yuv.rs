//! Shared I420 scaling/letterboxing used by every virtual-camera backend: the source phone frame
//! (any size) is fit into the backend's fixed output size (nearest neighbour, centred, black bars).
//! Linux writes the result straight to v4l2loopback (`v4l2out.rs`); Windows interleaves it to NV12
//! for the Media Foundation virtual camera (`vcam_windows.rs`).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

pub fn fill_black(frame: &mut [u8], w: usize, h: usize) {
    let (y, uv) = frame.split_at_mut(w * h);
    y.fill(16);
    uv.fill(128);
}

fn copy_plane(src: &[u8], stride: usize, dst: &mut [u8], w: usize, h: usize) {
    for row in 0..h {
        dst[row * w..row * w + w].copy_from_slice(&src[row * stride..row * stride + w]);
    }
}

/// Largest rectangle with the source aspect ratio centred in the destination, even-aligned.
pub fn fit(sw: usize, sh: usize, dw: usize, dh: usize) -> Rect {
    let (w, h) = if sw * dh > sh * dw { (dw, sh * dw / sw) } else { (sw * dh / sh, dh) };
    let (w, h) = (w & !1, h & !1);
    Rect { x: ((dw - w) / 2) & !1, y: ((dh - h) / 2) & !1, w, h }
}

fn scale_plane(src: &[u8], stride: usize, sw: usize, sh: usize, dst: &mut [u8], dstride: usize, r: Rect) {
    for dy in 0..r.h {
        let srow = &src[(dy * sh / r.h) * stride..];
        let drow = &mut dst[(r.y + dy) * dstride + r.x..];
        for dx in 0..r.w {
            drow[dx] = srow[dx * sw / r.w];
        }
    }
}

/// Fits the source I420 planes into a `dw`x`dh` I420 frame (`dst`, laid out Y then U then V,
/// `dw*dh*3/2` bytes), letterboxed with black. `dw`/`dh` must be even.
pub fn scale_i420(
    (sw, sh): (usize, usize),
    (ys, us, vs): (usize, usize, usize),
    y: &[u8],
    u: &[u8],
    v: &[u8],
    dst: &mut [u8],
    dw: usize,
    dh: usize,
) {
    let (y_out, uv_out) = dst.split_at_mut(dw * dh);
    let (u_out, v_out) = uv_out.split_at_mut((dw / 2) * (dh / 2));
    if (sw, sh) == (dw, dh) {
        copy_plane(y, ys, y_out, dw, dh);
        copy_plane(u, us, u_out, dw / 2, dh / 2);
        copy_plane(v, vs, v_out, dw / 2, dh / 2);
    } else {
        y_out.fill(16);
        u_out.fill(128);
        v_out.fill(128);
        let r = fit(sw, sh, dw, dh);
        scale_plane(y, ys, sw, sh, y_out, dw, r);
        let rc = Rect { x: r.x / 2, y: r.y / 2, w: r.w / 2, h: r.h / 2 };
        scale_plane(u, us, sw / 2, sh / 2, u_out, dw / 2, rc);
        scale_plane(v, vs, sw / 2, sh / 2, v_out, dw / 2, rc);
    }
}

/// Converts a packed I420 frame (`Y`, then `U`, then `V` planes) into NV12 (`Y`, then interleaved
/// `U`/`V`) in place into `nv12`. Both buffers are `w*h*3/2` bytes; `w`/`h` must be even. Media
/// Foundation wants NV12; the rest of the pipeline (and the Linux v4l2 path) is I420.
pub fn i420_to_nv12(i420: &[u8], nv12: &mut [u8], w: usize, h: usize) {
    let luma = w * h;
    let chroma = (w / 2) * (h / 2);
    nv12[..luma].copy_from_slice(&i420[..luma]);
    let (u, v) = i420[luma..].split_at(chroma);
    let uv = &mut nv12[luma..];
    for i in 0..chroma {
        uv[2 * i] = u[i];
        uv[2 * i + 1] = v[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i420_to_nv12_interleaves_chroma() {
        // 2x2 I420: 4 Y, 1 U, 1 V.
        let i420 = [10, 11, 12, 13, 200, 201];
        let mut nv12 = [0u8; 6];
        i420_to_nv12(&i420, &mut nv12, 2, 2);
        assert_eq!(nv12, [10, 11, 12, 13, 200, 201]);
        // 4x2 I420: 8 Y, 2 U, 2 V -> UV interleaved U0 V0 U1 V1.
        let i420b = [0, 1, 2, 3, 4, 5, 6, 7, 90, 91, 100, 101];
        let mut nv12b = [0u8; 12];
        i420_to_nv12(&i420b, &mut nv12b, 4, 2);
        assert_eq!(&nv12b[8..], &[90, 100, 91, 101]);
    }

    #[test]
    fn fit_letterboxes_portrait_into_landscape() {
        assert_eq!(fit(720, 1280, 1280, 720), Rect { x: 438, y: 0, w: 404, h: 720 });
        assert_eq!(fit(1280, 720, 1280, 720), Rect { x: 0, y: 0, w: 1280, h: 720 });
        assert_eq!(fit(1920, 1080, 1280, 720), Rect { x: 0, y: 0, w: 1280, h: 720 });
    }

    #[test]
    fn scale_plane_downsamples() {
        let src: Vec<u8> = (0..16).collect(); // 4x4
        let mut dst = vec![0u8; 4];
        scale_plane(&src, 4, 4, 4, &mut dst, 2, Rect { x: 0, y: 0, w: 2, h: 2 });
        assert_eq!(dst, vec![0, 2, 8, 10]);
    }
}
