//! Adapted from Clatter (<https://github.com/Ralith/clatter/>)
//! which is available under MIT/Apache-2.0/zlib

use fearless_simd::prelude::*;

use super::{
    grid::{self, Grid},
    hash,
};

#[inline(always)]
pub fn simplex_1d<S: Simd>(point: [S::f32s; 1], seed: i32) -> S::f32s {
    let ([[i0], [i1]], [[x0], [x1]]) = grid::Simplex.get::<S>(point);

    // Select gradients
    let gi0 = hash::hash_1d::<S>((i0 ^ seed).bitcast());
    let gi1 = hash::hash_1d::<S>((i1 ^ seed).bitcast());

    // Compute the contribution from the first gradient
    // n0 = grad0 * (1 - x0^2)^4 * x0
    // t0 = 1.0 - (x0 * x0);
    let t0 = (-x0).mul_add(x0, 1.0);
    let t20 = t0 * t0;
    let t40 = t20 * t20;
    let gx0 = gradient_1d::<S>(gi0);

    // Compute the contribution from the second gradient
    // n1 = grad1 * (x0 - 1) * (1 - (x0 - 1)^2)^4
    // t1 = 1.0 - (x1 * x1);
    let t1 = (-x1).mul_add(x1, 1.0);
    let t21 = t1 * t1;
    let t41 = t21 * t21;
    let gx1 = gradient_1d::<S>(gi1);

    // (t40 * gx0 * x0) + (t41 * gx1 * x1)
    (t40 * gx0).mul_add(x0, t41 * gx1 * x1)
}

/// Generates a nonzero random integer gradient in ±8 inclusive
#[inline(always)]
fn gradient_1d<S: Simd>(hash: S::u32s) -> S::f32s {
    let h = hash >> 28;
    let v = ((h & 7) + 1).bitcast::<S::i32s>().to_float::<S::f32s>();

    (v.bitcast::<S::u32s>() | (hash & (1 << 31))).bitcast::<S::f32s>()
}

#[inline(always)]
pub fn simplex_2d<S: Simd>(point: [S::f32s; 2], seed: i32) -> S::f32s {
    let ([[i0, j0], [i1, j1], [i2, j2]], [[x0, y0], [x1, y1], [x2, y2]]) =
        grid::Simplex.get::<S>(point);

    let seed = S::i32s::splat(i0.witness(), seed);
    let gi0 = hash::pcg_3d::<S>([i0, j0, seed].map(|x| x.bitcast()))[0];
    let gi1 = hash::pcg_3d::<S>([i1, j1, seed].map(|x| x.bitcast()))[0];
    let gi2 = hash::pcg_3d::<S>([i2, j2, seed].map(|x| x.bitcast()))[0];

    // Weights associated with the gradients at each corner
    // These FMA operations are equivalent to: let t = max(0, 0.5 - x*x - y*y)
    let t0 = y0.mul_add(-y0, x0.mul_add(-x0, 0.5f32)).max(0.0f32);
    let t1 = y1.mul_add(-y1, x1.mul_add(-x1, 0.5)).max(0.0);
    let t2 = y2.mul_add(-y2, x2.mul_add(-x2, 0.5)).max(0.0);

    let t20 = t0 * t0;
    let t40 = t20 * t20;
    let t21 = t1 * t1;
    let t41 = t21 * t21;
    let t22 = t2 * t2;
    let t42 = t22 * t22;

    let [gx0, gy0] = gradient_2d::<S>(gi0);
    let g0 = gx0 * x0 + gy0 * y0;
    let n0 = t40 * g0;
    let [gx1, gy1] = gradient_2d::<S>(gi1);
    let g1 = gx1 * x1 + gy1 * y1;
    let n1 = t41 * g1;
    let [gx2, gy2] = gradient_2d::<S>(gi2);
    let g2 = gx2 * x2 + gy2 * y2;
    let n2 = t42 * g2;

    n0 + n1 + n2
}

#[inline(always)]
fn gradient_2d<S: Simd>(hash: S::u32s) -> [S::f32s; 2] {
    let h = hash & 7;

    let mask = h.simd_lt(S::u32s::splat(hash.witness(), 4));
    let _f1 = S::f32s::splat(hash.witness(), 1.0);
    let _f2 = S::f32s::splat(hash.witness(), 2.0);
    let x_magnitude = mask.select(_f1, _f2);
    let y_magnitude = mask.select(_f2, _f1);

    let h_and_1 = (h & 1).simd_eq(0);
    let h_and_2 = (h & 2).simd_eq(0);

    let gx = mask
        .select(h_and_1, h_and_2)
        .select(x_magnitude, -x_magnitude);
    let gy = mask
        .select(h_and_2, h_and_1)
        .select(y_magnitude, -y_magnitude);
    [gx, gy]
}
