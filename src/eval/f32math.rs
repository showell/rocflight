//! Roc's binary32 transcendentals, ported from
//! `roc-compiler/src/builtins/float_math/f32.zig` so an `F32`'s bits agree with every
//! roc backend. Every operation here is binary32: argument reduction multiplies the
//! significand by a fixed-point 2/pi rather than widening to f64.

type Fixed = [u64; 5];

// floor((2/pi) * 2^256), least-significant limb first.
const TWO_OVER_PI: [u64; 4] =
    [0xfe51_63ab_debb_c561, 0xdb62_9599_3c43_9041, 0xfc27_57d1_f534_ddc0, 0xa2f9_836e_4e44_1529];
const PIO2_HI: f32 = 1.5707962513e+00;
const PIO2_LO: f32 = 7.5497894159e-08;

fn canonical_nan() -> f32 {
    f32::from_bits(0x7fc0_0000)
}

fn multiply_two_over_pi(significand: u32) -> Fixed {
    let mut product = [0u64; 5];
    let mut carry: u128 = 0;
    for (index, limb) in TWO_OVER_PI.iter().enumerate() {
        let wide = (*limb as u128) * (significand as u128) + carry;
        product[index] = wide as u64;
        carry = wide >> 64;
    }
    product[4] = carry as u64;
    product
}

fn fixed_bit(value: &Fixed, bit_index: u16) -> bool {
    if bit_index >= 320 {
        return false;
    }
    value[(bit_index / 64) as usize] & (1u64 << (bit_index % 64)) != 0
}

fn lower_bits(value: &Fixed, bit_count: u16) -> Fixed {
    let mut result = [0u64; 5];
    let whole = (bit_count / 64) as usize;
    let partial = bit_count % 64;
    result[..whole.min(5)].copy_from_slice(&value[..whole.min(5)]);
    if whole < 5 && partial != 0 {
        result[whole] = value[whole] & ((1u64 << partial) - 1);
    }
    result
}

fn power_of_two_minus(value: &Fixed, exponent: u16) -> Fixed {
    let mut result = [0u64; 5];
    result[(exponent / 64) as usize] = 1u64 << (exponent % 64);
    let mut borrow: u128 = 0;
    for (limb, subtrahend) in result.iter_mut().zip(value.iter()) {
        let subtrahend = *subtrahend as u128 + borrow;
        let minuend = *limb as u128;
        *limb = minuend.wrapping_sub(subtrahend) as u64;
        borrow = (minuend < subtrahend) as u128;
    }
    result
}

fn highest_set_bit(value: &Fixed) -> Option<u16> {
    (0..5).rev().find(|&i| value[i] != 0).map(|i| (i * 64 + (63 - value[i].leading_zeros() as usize)) as u16)
}

fn shifted_low_u32(value: &Fixed, shift: u16) -> u32 {
    if shift >= 320 {
        return 0;
    }
    let limb = (shift / 64) as usize;
    let bit = shift % 64;
    let mut result = value[limb] >> bit;
    if bit != 0 && limb + 1 < 5 {
        result |= value[limb + 1] << (64 - bit);
    }
    result as u32
}

fn any_bits_below(value: &Fixed, bit_count: u16) -> bool {
    let capped = bit_count.min(320);
    let whole = (capped / 64) as usize;
    let partial = capped % 64;
    if value[..whole].iter().any(|limb| *limb != 0) {
        return true;
    }
    whole < 5 && partial != 0 && value[whole] & ((1u64 << partial) - 1) != 0
}

fn rounded_shift(value: &Fixed, shift: i16) -> u32 {
    if shift <= 0 {
        return shifted_low_u32(value, 0) << ((-shift) as u32);
    }
    let right = shift as u16;
    let mut result = shifted_low_u32(value, right);
    let halfway = fixed_bit(value, right - 1);
    let below_halfway = any_bits_below(value, right - 1);
    if halfway && (below_halfway || result & 1 != 0) {
        result += 1;
    }
    result
}

fn fixed_fraction_to_f32(magnitude: &Fixed, denominator_exponent: u16) -> f32 {
    let Some(highest) = highest_set_bit(magnitude) else { return 0.0 };
    let mut unbiased = highest as i16 - denominator_exponent as i16;
    if unbiased >= -126 {
        let mut significand = rounded_shift(magnitude, highest as i16 - 23);
        if significand == 0x0100_0000 {
            significand >>= 1;
            unbiased += 1;
        }
        let biased = (unbiased + 127) as u32;
        return f32::from_bits((biased << 23) | (significand & 0x007f_ffff));
    }
    f32::from_bits(rounded_shift(magnitude, denominator_exponent as i16 - 149))
}

struct Reduction {
    quadrant: u8,
    remainder: f32,
}

fn reduce(value: f32) -> Reduction {
    let bits = value.to_bits();
    let abs_bits = bits & 0x7fff_ffff;
    let exponent_bits = (abs_bits >> 23) & 0xff;
    let significand = (abs_bits & 0x007f_ffff) | 0x0080_0000;
    let binary_exponent = exponent_bits as i16 - 150;
    let denominator_exponent = (256 - binary_exponent) as u16;
    let product = multiply_two_over_pi(significand);

    let rounds_up = fixed_bit(&product, denominator_exponent - 1);
    let mut quadrant = fixed_bit(&product, denominator_exponent) as u8
        | ((fixed_bit(&product, denominator_exponent + 1) as u8) << 1);
    if rounds_up {
        quadrant = (quadrant + 1) & 3;
    }
    let fraction = lower_bits(&product, denominator_exponent);
    let magnitude = if rounds_up { power_of_two_minus(&fraction, denominator_exponent) } else { fraction };

    let mut reduced = fixed_fraction_to_f32(&magnitude, denominator_exponent);
    let negative = bits >> 31 != 0;
    if rounds_up != negative {
        reduced = -reduced;
    }
    if negative {
        quadrant = 0u8.wrapping_sub(quadrant) & 3;
    }
    Reduction { quadrant, remainder: reduced * PIO2_HI + reduced * PIO2_LO }
}

fn sin_kernel(x: f32) -> f32 {
    if x.to_bits() & 0x7fff_ffff < 0x3980_0000 {
        return x;
    }
    let (s1, s2, s3, s4): (f32, f32, f32, f32) =
        (-1.6666667163e-01, 8.3333291113e-03, -1.9839334413e-04, 2.7183114939e-06);
    let z = x * x;
    x + x * z * (s1 + z * (s2 + z * (s3 + z * s4)))
}

fn cos_kernel(x: f32) -> f32 {
    let (c0, c1, c2, c3): (f32, f32, f32, f32) =
        (-4.9999997020e-01, 4.1666623205e-02, -1.3886763481e-03, 2.4390447366e-05);
    let z = x * x;
    let w = z * z;
    (1.0 + z * c0) + w * c1 + w * z * (c2 + z * c3)
}

fn sin_cos(value: f32) -> (f32, f32) {
    let abs_bits = value.to_bits() & 0x7fff_ffff;
    if abs_bits >= 0x7f80_0000 {
        let nan = value - value;
        return (nan, nan);
    }
    let reduction = if abs_bits <= 0x3f49_0fda { Reduction { quadrant: 0, remainder: value } } else { reduce(value) };
    let s = sin_kernel(reduction.remainder);
    let c = cos_kernel(reduction.remainder);
    match reduction.quadrant {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

pub fn sin(value: f32) -> f32 {
    sin_cos(value).0
}

pub fn cos(value: f32) -> f32 {
    sin_cos(value).1
}

pub fn tan(value: f32) -> f32 {
    let (s, c) = sin_cos(value);
    s / c
}

fn inverse_rational(z: f32) -> f32 {
    let (p0, p1, p2, q1): (f32, f32, f32, f32) =
        (1.6666586697e-01, -4.2743422091e-02, -8.6563630030e-03, -7.0662963390e-01);
    let numerator = z * (p0 + z * (p1 + z * p2));
    let denominator = 1.0 + z * q1;
    numerator / denominator
}

pub fn asin(value: f32) -> f32 {
    let bits = value.to_bits();
    let abs_bits = bits & 0x7fff_ffff;
    if abs_bits >= 0x3f80_0000 {
        if abs_bits == 0x3f80_0000 {
            return if bits >> 31 == 0 { PIO2_HI + PIO2_LO } else { -(PIO2_HI + PIO2_LO) };
        }
        return canonical_nan();
    }
    if abs_bits < 0x3f00_0000 {
        if abs_bits < 0x3980_0000 {
            return value;
        }
        return value + value * inverse_rational(value * value);
    }
    let z = (1.0 - value.abs()) * 0.5;
    let root = z.sqrt();
    let ratio = inverse_rational(z);
    let local = PIO2_HI - (2.0 * (root + root * ratio) - PIO2_LO);
    if bits >> 31 == 0 { local } else { -local }
}

pub fn acos(value: f32) -> f32 {
    let bits = value.to_bits();
    let abs_bits = bits & 0x7fff_ffff;
    if abs_bits >= 0x3f80_0000 {
        if abs_bits == 0x3f80_0000 {
            return if bits >> 31 == 0 { 0.0 } else { 2.0 * (PIO2_HI + PIO2_LO) };
        }
        return canonical_nan();
    }
    if abs_bits < 0x3f00_0000 {
        if abs_bits <= 0x3280_0000 {
            return PIO2_HI + PIO2_LO;
        }
        return PIO2_HI - (value - (PIO2_LO - value * inverse_rational(value * value)));
    }
    if bits >> 31 != 0 {
        let z = (1.0 + value) * 0.5;
        let root = z.sqrt();
        let correction = inverse_rational(z) * root - PIO2_LO;
        return 2.0 * (PIO2_HI - (root + correction));
    }
    let z = (1.0 - value) * 0.5;
    let root = z.sqrt();
    let root_hi = f32::from_bits(root.to_bits() & 0xffff_f000);
    let correction = (z - root_hi * root_hi) / (root + root_hi);
    let tail = inverse_rational(z) * root + correction;
    2.0 * (root_hi + tail)
}

pub fn atan(value: f32) -> f32 {
    const HIGH: [f32; 4] = [4.6364760399e-01, 7.8539812565e-01, 9.8279368877e-01, 1.5707962513e+00];
    const LOW: [f32; 4] = [5.0121582440e-09, 3.7748947079e-08, 3.4473217170e-08, 7.5497894159e-08];
    const COEFFICIENTS: [f32; 5] =
        [3.3333328366e-01, -1.9999158382e-01, 1.4253635705e-01, -1.0648017377e-01, 6.1687607318e-02];

    let bits = value.to_bits();
    let abs_bits = bits & 0x7fff_ffff;
    let negative = bits >> 31 != 0;
    if abs_bits >= 0x4c80_0000 {
        if abs_bits > 0x7f80_0000 {
            return value;
        }
        let result = HIGH[3] + LOW[3];
        return if negative { -result } else { result };
    }

    let reduced: f32;
    let mut identity: Option<usize> = None;
    if abs_bits < 0x3ee0_0000 {
        if abs_bits < 0x3980_0000 {
            return value;
        }
        reduced = value;
    } else {
        let magnitude = value.abs();
        if abs_bits < 0x3f98_0000 {
            if abs_bits < 0x3f30_0000 {
                reduced = (2.0 * magnitude - 1.0) / (2.0 + magnitude);
                identity = Some(0);
            } else {
                reduced = (magnitude - 1.0) / (magnitude + 1.0);
                identity = Some(1);
            }
        } else if abs_bits < 0x401c_0000 {
            reduced = (magnitude - 1.5) / (1.0 + 1.5 * magnitude);
            identity = Some(2);
        } else {
            reduced = -1.0 / magnitude;
            identity = Some(3);
        }
    }

    let z = reduced * reduced;
    let w = z * z;
    let odd = z * (COEFFICIENTS[0] + w * (COEFFICIENTS[2] + w * COEFFICIENTS[4]));
    let even = w * (COEFFICIENTS[1] + w * COEFFICIENTS[3]);
    if let Some(index) = identity {
        let result = HIGH[index] - ((reduced * (odd + even) - LOW[index]) - reduced);
        return if negative { -result } else { result };
    }
    reduced - reduced * (odd + even)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_roc_bits() {
        // From roc's "F32 transcendental exact bits agree across backends" test.
        assert_eq!(sin(f32::from_bits(0x3f49_0fda)).to_bits(), 0x3f35_04f3);
        assert_eq!(cos(f32::from_bits(0x3f49_0fda)).to_bits(), 0x3f35_04f4);
        assert_eq!(tan(f32::from_bits(0x3f49_0fda)).to_bits(), 0x3f7f_ffff);
        assert_eq!(sin(f32::from_bits(0x7f7f_ffff)).to_bits(), 0xbf05_99b3);
        assert_eq!(tan(f32::from_bits(0x3fc9_0fdb)).to_bits(), 0xcbae_8a4a);
        assert_eq!(asin(f32::from_bits(0x3f00_0001)).to_bits(), 0x3f06_0a94);
        assert_eq!(acos(f32::from_bits(0xbf40_0000)).to_bits(), 0x401a_ce94);
        assert_eq!(atan(f32::from_bits(0x401b_ffff)).to_bits(), 0x3f97_3ab9);
    }
}

/// Angle of (x, y), preserving IEEE signed zeros, infinities and NaNs: roc's
/// `float_math/f32.zig` `atan2`, the musl reduction around this file's `atan`.
pub fn atan2(y: f32, x: f32) -> f32 {
    const PI: f32 = 3.1415927410e+00;
    const PI_LO: f32 = -8.7422776573e-08;
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let (mut x_bits, mut y_bits) = (x.to_bits(), y.to_bits());
    if x_bits == 0x3F80_0000 {
        return atan(y);
    }
    // 2 * sign(x) + sign(y)
    let m = ((y_bits >> 31) & 1) | ((x_bits >> 30) & 2);
    x_bits &= 0x7FFF_FFFF;
    y_bits &= 0x7FFF_FFFF;
    if y_bits == 0 {
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    if x_bits == 0 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }
    if x_bits == 0x7F80_0000 {
        return if y_bits == 0x7F80_0000 {
            match m {
                0 => PI / 4.0,
                1 => -PI / 4.0,
                2 => 3.0 * PI / 4.0,
                _ => -3.0 * PI / 4.0,
            }
        } else {
            match m {
                0 => 0.0,
                1 => -0.0,
                2 => PI,
                _ => -PI,
            }
        };
    }
    // |y / x| > 0x1p26
    if x_bits + (26 << 23) < y_bits || y_bits == 0x7F80_0000 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }
    let z = if m & 2 != 0 && y_bits + (26 << 23) < x_bits { 0.0 } else { atan((y / x).abs()) };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}
