//! `math.fsum`: `CPython`'s exactly rounded floating-point sum (Modules/mathmodule.c
//! `math_fsum`, Shewchuk's partials with the half-even fix-up across partials), implemented
//! operation for operation so a track's `duration` — part of the snapshot digest — is the
//! same double Python computes; and the sequential sum of Python ≤ 3.11's `sum` that older
//! snapshots recorded.

/// Why `math.fsum` raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FsumError {
    /// `OverflowError("intermediate overflow in fsum")`.
    #[error("intermediate overflow in fsum")]
    Overflow,
    /// `ValueError("-inf + inf in fsum")`.
    #[error("-inf + inf in fsum")]
    InfMinusInf,
}

/// `math.fsum(values)`.
pub fn fsum(values: impl IntoIterator<Item = f64>) -> Result<f64, FsumError> {
    let mut partials: Vec<f64> = Vec::new();
    let mut special_sum = 0.0_f64;
    let mut inf_sum = 0.0_f64;
    for value in values {
        let mut x = value;
        let xsave = x;
        let mut kept = 0;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let yr = hi - x;
            let lo = y - yr;
            if lo != 0.0 {
                partials[kept] = lo;
                kept += 1;
            }
            x = hi;
        }
        partials.truncate(kept);
        if x != 0.0 {
            if x.is_finite() {
                partials.push(x);
            } else {
                // A non-finite x comes from an intermediate overflow or from a nan or an inf
                // among the summands.
                if xsave.is_finite() {
                    return Err(FsumError::Overflow);
                }
                if xsave.is_infinite() {
                    inf_sum += xsave;
                }
                special_sum += xsave;
                partials.clear();
            }
        }
    }
    if special_sum != 0.0 {
        return if inf_sum.is_nan() {
            Err(FsumError::InfMinusInf)
        } else {
            Ok(special_sum)
        };
    }
    Ok(sum_partials(&partials))
}

/// The exact sum of the partials from the top, stopping when it becomes inexact, with the
/// half-even fix-up (so `fsum([1e-16, 1, 1e16])` rounds to `1.0000000000000002e16`).
#[allow(clippy::float_cmp)] // exact comparisons, operation for operation as in mathmodule.c
fn sum_partials(partials: &[f64]) -> f64 {
    let mut n = partials.len();
    if n == 0 {
        return 0.0;
    }
    n -= 1;
    let mut hi = partials[n];
    let mut lo = 0.0_f64;
    while n > 0 {
        let x = hi;
        n -= 1;
        let y = partials[n];
        hi = x + y;
        let yr = hi - x;
        lo = y - yr;
        if lo != 0.0 {
            break;
        }
    }
    if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
        let y = lo * 2.0;
        let x = hi + y;
        let yr = x - hi;
        if y == yr {
            hi = x;
        }
    }
    hi
}

/// The left-to-right sum Python ≤ 3.11's `sum` computed (`total += value` from `0.0`).
pub fn sequential_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut total = 0.0_f64;
    for value in values {
        total += value;
    }
    total
}

/// `math.isclose(a, b, rel_tol=0, abs_tol=abs_tol)`.
#[allow(clippy::float_cmp)] // `a == b` is isclose's own exact shortcut
pub fn isclose_abs(a: f64, b: f64, abs_tol: f64) -> bool {
    if a == b {
        return true;
    }
    if a.is_infinite() || b.is_infinite() {
        return false;
    }
    (b - a).abs() <= abs_tol
}
