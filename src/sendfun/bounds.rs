//! The routable input range of each direction. The venue finds and certifies
//! it at each update.
//!
//! - **Upper bound:** the largest input that quotes, is not supply-capped,
//!   keeps the input vault's balance within `u64`, and stays on one side of
//!   each transfer-fee cap point. The output is not concave across a cap
//!   point. The range is the widest stretch between cap points that passes
//!   the checks.
//! - **Lower bound:** at least [`MIN_LOWER_BOUND`]. It doubles until the range
//!   passes Titan's construction, mean-value, price-monotone and monotone
//!   checks.

use crate::trading_venue::QuoteResult;
use crate::trading_venue::bounds::find_boundaries;
use crate::trading_venue::error::TradingVenueError;

use super::MIN_LOWER_BOUND;

/// The input range of one direction, or why it has none.
pub type Range = Result<(u64, u64), &'static str>;

/// Quotes one direction at an input amount.
pub trait Quoter {
    fn quote(&self, amount: u64) -> Result<QuoteResult, TradingVenueError>;
}

impl<F: Fn(u64) -> Result<QuoteResult, TradingVenueError>> Quoter for F {
    fn quote(&self, amount: u64) -> Result<QuoteResult, TradingVenueError> {
        self(amount)
    }
}

/// What bounds one direction besides its quotes.
pub struct Limits<'a> {
    /// The largest input that keeps the input vault's balance within `u64`.
    pub max_input: u64,
    /// The input transfer's cap point.
    pub input_cap: Option<u64>,
    /// What the output transfer lands at its cap point.
    pub output_at_cap: Option<u64>,
    pub quoter: &'a dyn Quoter,
}

/// A cap point: the output's slope steps up between `last_below` and
/// `first_above`. Inputs between the two are on neither side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kink {
    /// `None` if no input in the range is below the cap point.
    last_below: Option<u64>,
    /// `None` if no input in the range is past the cap point.
    first_above: Option<u64>,
}

/// The routable range of one direction. Fails with a static reason when no
/// range passes the checks.
pub fn range(limits: &Limits<'_>) -> Range {
    let within = |amount: u64| {
        let quote = limits.quoter.quote(amount);
        if amount > limits.max_input {
            return quote.map(|quote| QuoteResult {
                not_enough_liquidity: true,
                ..quote
            });
        }
        quote
    };
    let (lower, upper) = find_boundaries(&within).map_err(|_| "no input quotes")?;
    let lower = lower.max(MIN_LOWER_BOUND);
    if lower >= upper {
        return Err("no input of at least the minimum quotes");
    }

    let mut kinks = Vec::new();
    if let Some(cap) = limits.input_cap.filter(|cap| *cap > lower && *cap <= upper) {
        kinks.push(Kink {
            last_below: cap.checked_sub(1),
            first_above: Some(cap),
        });
    }
    if let Some(at_cap) = limits.output_at_cap {
        let received = |amount: u64| {
            limits
                .quoter
                .quote(amount)
                .map_or(0, |quote| quote.expected_output)
        };
        // An output that lands exactly `at_cap` may come from either side.
        if let Some(reaches) = first_where(lower, upper, |amount| received(amount) >= at_cap) {
            kinks.push(Kink {
                last_below: reaches.checked_sub(1).filter(|below| *below >= lower),
                first_above: first_where(lower, upper, |amount| received(amount) > at_cap),
            });
        }
    }
    kinks.sort_by_key(|kink| kink.first_above.unwrap_or(u64::MAX));

    segments(lower, upper, &kinks)
        .into_iter()
        .filter_map(|(start, end)| {
            certified_lower(limits.quoter, start, end).map(|lower| (lower, end))
        })
        .max_by_key(|(lower, end)| end.abs_diff(*lower))
        .ok_or("no input range passes the price checks")
}

/// The smallest input in `[lower, upper]` where `pass` holds. `pass` must hold
/// for every input above one where it holds.
fn first_where(lower: u64, upper: u64, pass: impl Fn(u64) -> bool) -> Option<u64> {
    if !pass(upper) {
        return None;
    }
    if pass(lower) {
        return Some(lower);
    }
    // `pass(below)` is false and `pass(at)` true.
    let (mut below, mut at) = (lower, upper);
    while let Some(gap) = at.checked_sub(below).filter(|gap| *gap > 1) {
        let mid = below.checked_add(gap / 2)?;
        if pass(mid) {
            at = mid;
        } else {
            below = mid;
        }
    }
    Some(at)
}

/// `[lower, upper]` split at each kink, without the inputs between a kink's
/// sides.
fn segments(lower: u64, upper: u64, kinks: &[Kink]) -> Vec<(u64, u64)> {
    let mut segments = Vec::new();
    let mut start = lower;
    for kink in kinks {
        if let Some(end) = kink.last_below.filter(|end| *end >= start) {
            segments.push((start, end));
        }
        let Some(first_above) = kink.first_above else {
            return segments;
        };
        start = start.max(first_above);
    }
    if start <= upper {
        segments.push((start, upper));
    }
    segments
}

/// The lower bound of `[start, end]`: `start`, doubled until the range passes
/// the checks.
fn certified_lower(quoter: &dyn Quoter, start: u64, end: u64) -> Option<u64> {
    let mut lower = start.max(MIN_LOWER_BOUND);
    while lower < end {
        if passes(quoter, lower, end) {
            return Some(lower);
        }
        lower = lower.checked_mul(2)?;
    }
    None
}

const MEAN_VALUE_POINTS: usize = 64;
/// As many points as Titan's price-monotone check samples.
const PRICE_POINTS: usize = 200;

/// Titan's construction, mean-value, price-monotone and monotone checks over
/// `[lb, ub]`.
pub fn passes(quoter: &dyn Quoter, lb: u64, ub: u64) -> bool {
    edges_quote(quoter, lb, ub)
        && mean_value_theorem(quoter, &geometric_grid(lb, ub, MEAN_VALUE_POINTS))
        && monotone(quoter, &geometric_grid(lb, ub, PRICE_POINTS))
}

/// Both edges quote in full, with output and a positive price.
fn edges_quote(quoter: &dyn Quoter, lb: u64, ub: u64) -> bool {
    [lb, ub].into_iter().all(|amount| {
        quoter.quote(amount).is_ok_and(|quote| {
            !quote.not_enough_liquidity && quote.expected_output > 0 && quote.price > 0.0
        })
    })
}

/// Titan's geometric grid: `n` points spaced evenly in `ln` over `[lb, ub]`,
/// sorted, without repeats. Empty unless `1 <= lb < ub` and `n >= 2`.
#[expect(
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "Titan's exact float steps: `exp` is positive, and each operation rounds"
)]
fn geometric_grid(lb: u64, ub: u64, n: usize) -> Vec<u64> {
    let Some(last) = n.checked_sub(1).filter(|last| *last >= 1) else {
        return Vec::new();
    };
    if lb < 1 || ub <= lb {
        return Vec::new();
    }
    let ln_lo = (lb as f64).ln();
    let ln_hi = (ub as f64).ln();
    let mut points: Vec<u64> = (0..n)
        .map(|i| {
            let t = i as f64 / last as f64;
            // Titan clamps the cast into `[lb, ub]`.
            let point = (ln_lo + t * (ln_hi - ln_lo)).exp() as u64;
            if point < lb {
                lb
            } else if point > ub {
                ub
            } else {
                point
            }
        })
        .collect();
    points.sort_unstable();
    points.dedup();
    points
}

/// Titan's mean-value check over adjacent points of `grid`.
#[expect(
    clippy::suboptimal_flops,
    reason = "Titan's check rounds each operation, which a fused multiply-add does not"
)]
fn mean_value_theorem(quoter: &dyn Quoter, grid: &[u64]) -> bool {
    const REL_TOL: f64 = 1e-5;
    const OUT_QUANTUM: f64 = 2.0;
    grid.windows(2).all(|pair| {
        let &[a, b] = pair else {
            return false;
        };
        let Some(width) = b.checked_sub(a).filter(|width| *width > 0) else {
            return true;
        };
        let (Ok(qa), Ok(qb)) = (quoter.quote(a), quoter.quote(b)) else {
            return false;
        };
        // A flat step carries no rate.
        let Some(rise) = qb
            .expected_output
            .checked_sub(qa.expected_output)
            .filter(|rise| *rise > 0)
        else {
            return true;
        };
        let chord = rise as f64 / width as f64;
        let (price_a, price_b) = (qa.price, qb.price);
        let atol = OUT_QUANTUM / width as f64;
        price_b <= price_a * (1.0 + REL_TOL)
            && chord <= price_a * (1.0 + REL_TOL) + atol
            && chord >= price_b * (1.0 - REL_TOL) - atol
    })
}

/// Titan's price-monotone and output-monotone checks over `amounts`, which
/// ascend.
fn monotone(quoter: &dyn Quoter, amounts: &[u64]) -> bool {
    const REL_TOL: f64 = 1e-3;
    let mut prev_price = f64::INFINITY;
    let mut prev_output = 0;
    amounts.iter().all(|&amount| {
        let Ok(quote) = quoter.quote(amount) else {
            return false;
        };
        let ok = quote.price > 0.0
            && quote.price <= prev_price * (1.0 + REL_TOL)
            && prev_output <= quote.expected_output;
        prev_price = quote.price;
        prev_output = quote.expected_output;
        ok
    })
}
