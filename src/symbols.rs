//! Single-width status glyphs used across command output.
//!
//! All constants are guaranteed single-width and free of emoji presentation so
//! that they line up in fixed-width and tabular output. Color and bold styling
//! are applied at the call site, not here.

/// Success marker.
pub const OK: &str = "✓"; // U+2713 CHECK MARK
/// Failure marker.
pub const ERR: &str = "✗"; // U+2717 BALLOT X
/// Informational bullet.
pub const BULLET: &str = "•"; // U+2022 BULLET
/// State-transition marker.
pub const TRANSITION: &str = "→"; // U+2192 RIGHTWARDS ARROW

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_single_scalar_values() {
        for s in [OK, ERR, BULLET, TRANSITION] {
            assert_eq!(
                s.chars().count(),
                1,
                "{s:?} must be a single scalar value"
            );
        }
    }

    #[test]
    fn glyphs_are_the_expected_code_points() {
        // Guard against someone swapping in an emoji-presentation glyph (e.g. the
        // double-width ⏸ that TRANSITION replaced).
        assert_eq!(OK, "\u{2713}");
        assert_eq!(ERR, "\u{2717}");
        assert_eq!(BULLET, "\u{2022}");
        assert_eq!(TRANSITION, "\u{2192}");
    }
}
