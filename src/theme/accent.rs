//! An accent colour taken from a picture, the way Material You scores wallpaper colours: hues
//! are grouped, and the group that both covers much of the picture and is vivid wins. The
//! result is set to a tone that reads on a dark surface. Works in OKLCH, so hues and chroma are
//! compared the way they look.

use super::Color;

const BINS: usize = 36;

/// Linear sRGB of one channel.
fn linear(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB → OKLab.
fn oklab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab → linear sRGB (may be out of gamut).
fn linear_rgb(lab: [f32; 3]) -> [f32; 3] {
    let l = (lab[0] + 0.396_337_78 * lab[1] + 0.215_803_76 * lab[2]).powi(3);
    let m = (lab[0] - 0.105_561_346 * lab[1] - 0.063_854_17 * lab[2]).powi(3);
    let s = (lab[0] - 0.089_484_18 * lab[1] - 1.291_485_5 * lab[2]).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// The colour of lightness `l`, chroma `c` and hue `h` (radians), with the chroma lowered until
/// it fits sRGB.
fn from_lch(l: f32, mut c: f32, h: f32) -> Color {
    loop {
        let rgb = linear_rgb([l, c * h.cos(), c * h.sin()]);
        if rgb.iter().all(|v| (-0.0001..=1.0001).contains(v)) || c <= 0.0 {
            let byte = |v: f32| (encode(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
            return Color(byte(rgb[0]), byte(rgb[1]), byte(rgb[2]));
        }
        c -= 0.005;
    }
}

/// The accent for pixels given as RGBA bytes, or `None` for a picture without colour (greys).
pub fn accent_of(rgba: &[u8]) -> Option<Color> {
    // Per hue bin: how many pixels, their chroma, and the direction of their hues.
    let mut count = [0.0f32; BINS];
    let mut chroma = [0.0f32; BINS];
    let mut direction = [[0.0f32; 2]; BINS];
    let mut total = 0.0f32;
    for pixel in rgba.as_chunks::<4>().0 {
        if pixel[3] < 128 {
            continue;
        }
        let [l, a, b] = oklab(pixel[0], pixel[1], pixel[2]);
        total += 1.0;
        let c = (a * a + b * b).sqrt();
        // Near-greys, near-black and near-white carry no hue.
        if c < 0.04 || !(0.15..=0.97).contains(&l) {
            continue;
        }
        let h = b.atan2(a);
        let bin = ((h.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * BINS as f32)
            as usize)
            .min(BINS - 1);
        count[bin] += 1.0;
        chroma[bin] += c;
        direction[bin][0] += c * h.cos();
        direction[bin][1] += c * h.sin();
    }
    if total == 0.0 {
        return None;
    }
    // A group is a bin with its neighbours, so a hue spread over two bins is not split.
    let group = |i: usize| [(i + BINS - 1) % BINS, i, (i + 1) % BINS];
    let score = |i: usize| {
        let (n, c) = group(i).iter().fold((0.0, 0.0), |(n, c), &j| (n + count[j], c + chroma[j]));
        if n < total * 0.01 {
            return f32::MIN;
        }
        let proportion = n / total;
        let vividness = c / n;
        // Material's score: mostly coverage, plus chroma above a moderate level.
        proportion * 70.0
            + (vividness * 300.0 - 48.0) * if vividness * 300.0 >= 48.0 { 0.3 } else { 0.1 }
    };
    let best = (0..BINS).max_by(|&a, &b| score(a).total_cmp(&score(b)))?;
    if score(best) == f32::MIN {
        return None;
    }
    let (n, c, x, y) = group(best).iter().fold((0.0, 0.0, 0.0, 0.0), |(n, c, x, y), &j| {
        (n + count[j], c + chroma[j], x + direction[j][0], y + direction[j][1])
    });
    let hue = y.atan2(x);
    let vividness = c / n;
    // A light tone, colourful but not loud, for text and fills on dark surfaces.
    Some(from_lch(0.80, vividness.clamp(0.08, 0.14), hue))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(colours: &[([u8; 3], usize)]) -> Vec<u8> {
        colours
            .iter()
            .flat_map(|(rgb, n)| std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], *n))
            .flatten()
            .collect()
    }

    #[test]
    fn the_large_vivid_hue_wins() {
        let pink = [236, 130, 170];
        let grey = [90, 90, 96];
        let accent = accent_of(&picture(&[(pink, 400), (grey, 500), ([30, 160, 60], 20)])).unwrap();
        let [_, a, b] = oklab(accent.0, accent.1, accent.2);
        let [_, pa, pb] = oklab(pink[0], pink[1], pink[2]);
        // Same hue as the pink, within a few degrees.
        assert!((b.atan2(a) - pb.atan2(pa)).abs() < 0.12);
    }

    #[test]
    fn greys_have_no_accent() {
        assert_eq!(accent_of(&picture(&[([128, 128, 128], 100), ([20, 20, 20], 100)])), None);
    }

    #[test]
    fn colours_stay_in_gamut() {
        let Color(r, g, b) = from_lch(0.8, 0.3, 2.0);
        assert!(r > 0 || g > 0 || b > 0);
    }
}
