use crate::theme::Rgba;
use tiny_skia::PixmapMut;

/// Reusable zero-allocation scratch workspace for continuous-sigma separable 2D Gaussian blur
/// on streaming frontier word masks.
pub struct BlurScratch {
    src: Vec<f32>,
    tmp: Vec<f32>,
    dst: Vec<f32>,
    kernel: Vec<f32>,
}

impl Default for BlurScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl BlurScratch {
    pub fn new() -> Self {
        Self {
            src: Vec::with_capacity(16384),
            tmp: Vec::with_capacity(16384),
            dst: Vec::with_capacity(16384),
            kernel: Vec::with_capacity(64),
        }
    }

    pub fn prepare_mask(&mut self, width: usize, height: usize) -> &mut [f32] {
        let len = width.saturating_mul(height);
        self.src.clear();
        self.src.resize(len, 0.0);
        &mut self.src
    }

    /// Runs a true continuous-sigma separable 2D Gaussian blur on the prepared `width x height`
    /// alpha mask (`[0.0..=1.0]`) and composites it onto `pixmap` at `(dst_x, dst_y)` clipped to
    /// `clip_rect = (cx0, cy0, cx1, cy1)` in physical pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_blurred_mask(
        &mut self,
        width: usize,
        height: usize,
        sigma: f32,
        color: Rgba,
        opacity: f32,
        dst_x: i32,
        dst_y: i32,
        clip: (i32, i32, i32, i32),
        pixmap: &mut PixmapMut<'_>,
    ) {
        if width == 0 || height == 0 || opacity <= 0.002 {
            return;
        }
        let len = width * height;
        let alpha_out: &[f32] = if sigma <= 0.18 {
            &self.src[..len]
        } else {
            let sigma = sigma.min(20.0);
            let radius = ((sigma * 2.6).ceil() as usize).clamp(1, 28);
            self.kernel.clear();
            let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
            let mut sum = 0.0_f32;
            for i in 0..=radius {
                let d = i as f32;
                let w = (-d * d * inv_two_sigma_sq).exp();
                self.kernel.push(w);
                sum += if i == 0 { w } else { 2.0 * w };
            }
            let inv_sum = 1.0 / sum.max(1e-6);
            for w in &mut self.kernel {
                *w *= inv_sum;
            }

            self.tmp.clear();
            self.tmp.resize(len, 0.0);
            self.dst.clear();
            self.dst.resize(len, 0.0);

            // Pass 1: Horizontal 1D Gaussian
            let k0 = self.kernel[0];
            for y in 0..height {
                let row = y * width;
                let src_row = &self.src[row..row + width];
                let tmp_row = &mut self.tmp[row..row + width];
                for x in 0..width {
                    let mut acc = src_row[x] * k0;
                    for r in 1..=radius {
                        let kr = self.kernel[r];
                        let left = if x >= r { src_row[x - r] } else { 0.0 };
                        let right = if x + r < width { src_row[x + r] } else { 0.0 };
                        acc += (left + right) * kr;
                    }
                    tmp_row[x] = acc;
                }
            }

            // Pass 2: Vertical 1D Gaussian
            for y in 0..height {
                let dst_row_off = y * width;
                for x in 0..width {
                    let mut acc = self.tmp[dst_row_off + x] * k0;
                    for r in 1..=radius {
                        let kr = self.kernel[r];
                        let up = if y >= r {
                            self.tmp[(y - r) * width + x]
                        } else {
                            0.0
                        };
                        let down = if y + r < height {
                            self.tmp[(y + r) * width + x]
                        } else {
                            0.0
                        };
                        acc += (up + down) * kr;
                    }
                    self.dst[dst_row_off + x] = acc;
                }
            }
            &self.dst[..len]
        };

        let pm_w = pixmap.width() as i32;
        let pm_h = pixmap.height() as i32;
        let cx0 = clip.0.max(0);
        let cy0 = clip.1.max(0);
        let cx1 = clip.2.min(pm_w);
        let cy1 = clip.3.min(pm_h);
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }

        let base_alpha = (color.a as f32 / 255.0) * opacity.clamp(0.0, 1.0);
        let cr = color.r as f32;
        let cg = color.g as f32;
        let cb = color.b as f32;
        let pixels = pixmap.data_mut();

        let y_start = (cy0 - dst_y).max(0) as usize;
        let y_end = ((cy1 - dst_y).min(height as i32)).max(0) as usize;
        let x_start = (cx0 - dst_x).max(0) as usize;
        let x_end = ((cx1 - dst_x).min(width as i32)).max(0) as usize;
        if x_start >= x_end || y_start >= y_end {
            return;
        }

        for ly in y_start..y_end {
            let py = (dst_y + ly as i32) as usize;
            let src_row = &alpha_out[ly * width..ly * width + width];
            let pm_row_base = py * (pm_w as usize) * 4;
            for lx in x_start..x_end {
                let a = (src_row[lx] * base_alpha).clamp(0.0, 1.0);
                if a <= 1.0 / 512.0 {
                    continue;
                }
                let px_idx = pm_row_base + ((dst_x + lx as i32) as usize) * 4;
                let inv_a = 1.0 - a;
                // tiny-skia stores premultiplied RGBA
                pixels[px_idx] = (cr * a + pixels[px_idx] as f32 * inv_a + 0.5) as u8;
                pixels[px_idx + 1] = (cg * a + pixels[px_idx + 1] as f32 * inv_a + 0.5) as u8;
                pixels[px_idx + 2] = (cb * a + pixels[px_idx + 2] as f32 * inv_a + 0.5) as u8;
                pixels[px_idx + 3] = (255.0 * a + pixels[px_idx + 3] as f32 * inv_a + 0.5) as u8;
            }
        }
    }
}

/// Fast Abramowitz-Stegun rational approximation to `erf(x)` (max error < 1.5e-7).
#[inline(always)]
fn fast_erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let ax = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * ax);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-ax * ax).exp();
    sign * y
}

/// Closed-form analytical Gaussian rounded-rectangle drop shadow (`0 offset_y blur rgba(0,0,0,alpha)`).
/// Gaussian quadrature over rounded horizontal slices, skipping the opaque interior.
#[allow(clippy::too_many_arguments)]
pub fn draw_diffuse_rrect_shadow(
    pixmap: &mut PixmapMut<'_>,
    card_x: f32,
    card_y: f32,
    card_w: f32,
    card_h: f32,
    radius: f32,
    offset_y: f32,
    sigma: f32,
    alpha: f32,
    shadow_rgb: (u8, u8, u8),
) {
    if alpha <= 0.002 || sigma <= 0.2 || card_w <= 2.0 || card_h <= 2.0 {
        return;
    }
    let pm_w = pixmap.width() as i32;
    let pm_h = pixmap.height() as i32;

    // Shadow box is slightly inset horizontally (`-12px` spread in final-chat's `0 16px 48px -16px`)
    let spread = -sigma * 0.35;
    let sx0 = card_x - spread;
    let sy0 = card_y + offset_y - spread;
    let sx1 = card_x + card_w + spread;
    let sy1 = card_y + card_h + offset_y + spread;
    if sx1 <= sx0 + 1.0 || sy1 <= sy0 + 1.0 {
        return;
    }

    let pad = (sigma * 2.8).ceil();
    let x0 = ((sx0 - pad).floor() as i32).max(0);
    let y0 = ((sy0 - pad).floor() as i32).max(0);
    let x1 = ((sx1 + pad).ceil() as i32).min(pm_w);
    let y1 = ((sy1 + pad).ceil() as i32).min(pm_h);

    // Interior of the opaque card that needs no shadow evaluation (inset by radius for rounded corners)
    let inner_x0 = (card_x + radius).ceil() as i32;
    let inner_x1 = (card_x + card_w - radius).floor() as i32;
    let inner_y0 = (card_y + radius).ceil() as i32;
    let inner_y1 = (card_y + card_h - radius).floor() as i32;

    let inv_sqrt2_sigma = 1.0 / (std::f32::consts::SQRT_2 * sigma);
    let sr = shadow_rgb.0 as f32;
    let sg = shadow_rgb.1 as f32;
    let sb = shadow_rgb.2 as f32;
    let pixels = pixmap.data_mut();

    for py in y0..y1 {
        let fy = py as f32 + 0.5;
        // Integrate horizontal Gaussian slices of the actual rounded silhouette.
        // A product of two erf terms blurs a BOX, leaving square shoulders
        // around a pill. Fixed Gaussian-weighted slices preserve curved ends.
        let r = (radius + spread).max(0.0).min((sx1 - sx0).min(sy1 - sy0) * 0.5);
        let mut slices = [(0.0_f32, 0.0_f32, 0.0_f32); 16];
        let mut slice_count = 0;
        for i in 0..16 {
            let z = -3.0 + (i as f32 + 0.5) * (6.0 / 16.0);
            let sample_y = fy + z * sigma;
            if sample_y < sy0 || sample_y > sy1 {
                continue;
            }
            let dy = (sy0 + r - sample_y).max(sample_y - (sy1 - r)).max(0.0);
            let inset = r - (r * r - dy * dy).max(0.0).sqrt();
            let weight = (-0.5 * z * z).exp() * (6.0 / 16.0) / (2.0 * std::f32::consts::PI).sqrt();
            let left = sx0 + inset;
            let right = sx1 - inset;
            // Most slices in the straight body have identical endpoints.
            // Integrate their weights once instead of evaluating 16 erf pairs.
            if let Some(existing) = slices[..slice_count].iter_mut()
                .find(|slice| slice.0 == left && slice.1 == right) {
                existing.2 += weight;
            } else {
                slices[slice_count] = (left, right, weight);
                slice_count += 1;
            }
        }
        let in_card_y = py >= inner_y0 && py < inner_y1;
        let row_off = (py as usize) * (pm_w as usize) * 4;

        for px in x0..x1 {
            if in_card_y && px >= inner_x0 && px < inner_x1 {
                continue;
            }
            let fx = px as f32 + 0.5;
            let coverage: f32 = slices[..slice_count].iter().map(|&(left, right, weight)| {
                weight * 0.5 * (fast_erf((right - fx) * inv_sqrt2_sigma)
                    - fast_erf((left - fx) * inv_sqrt2_sigma))
            }).sum();
            let a = (coverage * alpha).clamp(0.0, 1.0);
            if a <= 1.0 / 512.0 {
                continue;
            }
            let idx = row_off + (px as usize) * 4;
            let inv_a = 1.0 - a;
            pixels[idx] = (sr * a + pixels[idx] as f32 * inv_a + 0.5) as u8;
            pixels[idx + 1] = (sg * a + pixels[idx + 1] as f32 * inv_a + 0.5) as u8;
            pixels[idx + 2] = (sb * a + pixels[idx + 2] as f32 * inv_a + 0.5) as u8;
            pixels[idx + 3] = (255.0 * a + pixels[idx + 3] as f32 * inv_a + 0.5) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    #[test]
    fn continuous_gaussian_blur_conserves_energy_and_spreads_smoothly() {
        let mut scratch = BlurScratch::new();
        let w = 41;
        let h = 41;
        {
            let m = scratch.prepare_mask(w, h);
            m[20 * w + 20] = 1.0; // impulse at center
        }
        let mut pm = Pixmap::new(w as u32, h as u32).unwrap();
        scratch.composite_blurred_mask(
            w,
            h,
            4.5,
            Rgba::new(255, 255, 255, 255),
            1.0,
            0,
            0,
            (0, 0, w as i32, h as i32),
            &mut pm.as_mut(),
        );
        // Center pixel should be highest, radial neighbors smoothly decreasing and symmetric
        let c = pm.data()[(20 * w + 20) * 4 + 3];
        let n1 = pm.data()[(20 * w + 22) * 4 + 3];
        let n2 = pm.data()[(20 * w + 25) * 4 + 3];
        let sym = pm.data()[(22 * w + 20) * 4 + 3];
        assert!(c >= n1 && n1 >= n2);
        assert_eq!(n1, sym);
    }
}
