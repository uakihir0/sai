//! Primitive scalar literal checks shared by body type checking and constants.

use sai_hir::literals::Lit;
use sai_intern::Interner;
use sai_resolve::prelude::PrimTy;

/// Result of checking one direct scalar literal against a known primitive type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveLiteralResult {
    /// The literal has the expected primitive type and is in range.
    Match,
    /// The literal has a different primitive kind.
    TypeMismatch,
    /// The literal has the expected numeric kind but is out of range.
    OutOfRange,
}

/// Whether `primitive` is a type accepted by this scalar-literal checker.
pub const fn supports_primitive_literal(primitive: PrimTy) -> bool {
    matches!(
        primitive,
        PrimTy::I8
            | PrimTy::I16
            | PrimTy::I32
            | PrimTy::I64
            | PrimTy::U8
            | PrimTy::U16
            | PrimTy::U32
            | PrimTy::U64
            | PrimTy::ISize
            | PrimTy::USize
            | PrimTy::F32
            | PrimTy::F64
            | PrimTy::Bool
            | PrimTy::Byte
            | PrimTy::Char
    )
}

/// Whether `primitive` belongs to the integer literal family.
pub const fn is_integer_primitive(primitive: PrimTy) -> bool {
    matches!(
        primitive,
        PrimTy::I8
            | PrimTy::I16
            | PrimTy::I32
            | PrimTy::I64
            | PrimTy::U8
            | PrimTy::U16
            | PrimTy::U32
            | PrimTy::U64
            | PrimTy::ISize
            | PrimTy::USize
    )
}

/// Check a direct HIR scalar literal against a resolved primitive type.
///
/// The caller remains responsible for resolving the expected type and for
/// attaching source locations and diagnostic wording to the result.
pub fn check_primitive_literal(
    expected: PrimTy,
    literal: Lit<'_>,
    symbols: &Interner,
    pointer_width: u8,
) -> PrimitiveLiteralResult {
    match (expected, literal) {
        (PrimTy::Bool, Lit::Bool(_))
        | (PrimTy::Char, Lit::Char(_))
        | (PrimTy::Byte, Lit::Byte(_)) => PrimitiveLiteralResult::Match,
        (primitive, Lit::Int { value, negated }) if is_integer_primitive(primitive) => {
            if integer_literal_fits(primitive, value, negated, pointer_width) {
                PrimitiveLiteralResult::Match
            } else {
                PrimitiveLiteralResult::OutOfRange
            }
        }
        (primitive @ (PrimTy::F32 | PrimTy::F64), Lit::Float { text, negated }) => {
            let finite = symbols
                .resolve(text)
                .ok()
                .as_deref()
                .and_then(|text| round_float_literal(primitive, text, negated))
                .is_some_and(FloatLiteralValue::is_finite);
            if finite {
                PrimitiveLiteralResult::Match
            } else {
                PrimitiveLiteralResult::OutOfRange
            }
        }
        _ => PrimitiveLiteralResult::TypeMismatch,
    }
}

fn integer_literal_fits(
    primitive: PrimTy,
    value: Option<u128>,
    negated: bool,
    pointer_width: u8,
) -> bool {
    let Some(value) = value else {
        return false;
    };
    let (signed, width) = match primitive {
        PrimTy::I8 => (true, 8),
        PrimTy::I16 => (true, 16),
        PrimTy::I32 => (true, 32),
        PrimTy::I64 => (true, 64),
        PrimTy::U8 => (false, 8),
        PrimTy::U16 => (false, 16),
        PrimTy::U32 => (false, 32),
        PrimTy::U64 => (false, 64),
        PrimTy::ISize => (true, pointer_width),
        PrimTy::USize => (false, pointer_width),
        _ => return false,
    };
    if !matches!(width, 8 | 16 | 32 | 64) {
        return false;
    }
    if signed {
        let negative_limit = 1u128 << (width - 1);
        if negated {
            value <= negative_limit
        } else {
            value < negative_limit
        }
    } else if negated {
        value == 0
    } else {
        value <= (1u128 << width) - 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FloatLiteralValue {
    F32(f32),
    F64(f64),
}

impl FloatLiteralValue {
    fn is_finite(self) -> bool {
        match self {
            Self::F32(value) => value.is_finite(),
            Self::F64(value) => value.is_finite(),
        }
    }
}

fn round_float_literal(primitive: PrimTy, text: &str, negated: bool) -> Option<FloatLiteralValue> {
    match primitive {
        PrimTy::F32 => {
            let value = text.parse::<f32>().ok()?;
            Some(FloatLiteralValue::F32(if negated { -value } else { value }))
        }
        PrimTy::F64 => {
            let value = text.parse::<f64>().ok()?;
            Some(FloatLiteralValue::F64(if negated { -value } else { value }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FloatLiteralValue, PrimitiveLiteralResult, check_primitive_literal, integer_literal_fits,
        round_float_literal,
    };
    use sai_hir::literals::Lit;
    use sai_intern::Interner;
    use sai_resolve::prelude::PrimTy;

    #[test]
    fn primitive_literal_kinds_must_match_exactly() {
        let symbols = Interner::new();

        for (primitive, literal) in [
            (PrimTy::Bool, Lit::Bool(true)),
            (PrimTy::Char, Lit::Char('x')),
            (PrimTy::Byte, Lit::Byte(b'x')),
        ] {
            assert_eq!(
                check_primitive_literal(primitive, literal, &symbols, 64),
                PrimitiveLiteralResult::Match
            );
        }

        for (primitive, literal) in [
            (
                PrimTy::Bool,
                Lit::Int {
                    value: Some(1),
                    negated: false,
                },
            ),
            (PrimTy::Char, Lit::Byte(b'x')),
            (PrimTy::Byte, Lit::Char('x')),
            (
                PrimTy::I32,
                Lit::Float {
                    text: symbols.intern("1.0").unwrap(),
                    negated: false,
                },
            ),
            (
                PrimTy::F32,
                Lit::Int {
                    value: Some(1),
                    negated: false,
                },
            ),
            (PrimTy::Never, Lit::Bool(true)),
            (PrimTy::Array, Lit::Bool(true)),
        ] {
            assert_eq!(
                check_primitive_literal(primitive, literal, &symbols, 64),
                PrimitiveLiteralResult::TypeMismatch
            );
        }
    }

    #[test]
    fn integer_literals_use_signed_unsigned_and_missing_value_bounds() {
        let symbols = Interner::new();
        for (primitive, sign_bit) in [
            (PrimTy::I8, 7),
            (PrimTy::I16, 15),
            (PrimTy::I32, 31),
            (PrimTy::I64, 63),
        ] {
            let minimum_magnitude = 1u128 << sign_bit;
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(minimum_magnitude),
                        negated: true
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::Match,
                "{primitive:?} minimum"
            );
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(minimum_magnitude),
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::OutOfRange,
                "{primitive:?} positive limit"
            );
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(minimum_magnitude - 1),
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::Match,
                "{primitive:?} maximum"
            );
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(minimum_magnitude + 1),
                        negated: true
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::OutOfRange,
                "{primitive:?} negative overflow"
            );
        }
        for (primitive, width) in [
            (PrimTy::U8, 8),
            (PrimTy::U16, 16),
            (PrimTy::U32, 32),
            (PrimTy::U64, 64),
        ] {
            let maximum = (1u128 << width) - 1;
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(maximum),
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::Match,
                "{primitive:?} maximum"
            );
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(maximum + 1),
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::OutOfRange,
                "{primitive:?} overflow"
            );
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(0),
                        negated: true
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::Match,
                "{primitive:?} negative zero"
            );
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Int {
                        value: Some(1),
                        negated: true
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::OutOfRange,
                "{primitive:?} negative value"
            );
        }
        assert_eq!(
            check_primitive_literal(
                PrimTy::I64,
                Lit::Int {
                    value: None,
                    negated: false
                },
                &symbols,
                64
            ),
            PrimitiveLiteralResult::OutOfRange
        );
    }

    #[test]
    fn pointer_sized_integer_literals_use_the_selected_width() {
        for (width, signed_min, signed_max, unsigned_max) in [
            (32, 1u128 << 31, (1u128 << 31) - 1, (1u128 << 32) - 1),
            (64, 1u128 << 63, (1u128 << 63) - 1, (1u128 << 64) - 1),
        ] {
            assert!(integer_literal_fits(
                PrimTy::ISize,
                Some(signed_min),
                true,
                width
            ));
            assert!(integer_literal_fits(
                PrimTy::ISize,
                Some(signed_max),
                false,
                width
            ));
            assert!(!integer_literal_fits(
                PrimTy::ISize,
                Some(signed_min),
                false,
                width
            ));
            assert!(integer_literal_fits(
                PrimTy::USize,
                Some(unsigned_max),
                false,
                width
            ));
            assert!(!integer_literal_fits(
                PrimTy::USize,
                Some(unsigned_max + 1),
                false,
                width
            ));
        }
        assert!(!integer_literal_fits(PrimTy::USize, Some(0), false, 128));
    }

    #[test]
    fn float_literals_are_parsed_and_checked_after_target_rounding() {
        let symbols = Interner::new();
        for (primitive, text) in [
            (PrimTy::F32, "1.000000059604644775390625"),
            (PrimTy::F32, "1.000000059604644775390626"),
            (
                PrimTy::F64,
                "1.00000000000000011102230246251565404236316680908203125",
            ),
            (PrimTy::F32, "1e-50"),
            (PrimTy::F64, "1e-400"),
        ] {
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Float {
                        text: symbols.intern(text).unwrap(),
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::Match,
                "{primitive:?} {text}"
            );
        }
        for (primitive, text) in [
            (PrimTy::F32, "3.4028236e38"),
            (PrimTy::F64, "1.7976931348623159e308"),
        ] {
            assert_eq!(
                check_primitive_literal(
                    primitive,
                    Lit::Float {
                        text: symbols.intern(text).unwrap(),
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::OutOfRange,
                "{primitive:?} {text}"
            );
        }
        for text in [
            symbols.intern("not a decimal").unwrap(),
            sai_intern::Symbol::ERROR,
        ] {
            assert_eq!(
                check_primitive_literal(
                    PrimTy::F32,
                    Lit::Float {
                        text,
                        negated: false
                    },
                    &symbols,
                    64
                ),
                PrimitiveLiteralResult::OutOfRange,
                "invalid or unresolved float text must fail closed"
            );
        }
    }

    #[test]
    fn float_rounding_preserves_ties_to_even_without_double_rounding() {
        assert_eq!(
            round_float_literal(PrimTy::F32, "1.000000059604644775390625", false),
            Some(FloatLiteralValue::F32(1.0)),
            "the exact f32 midpoint rounds to the even lower significand"
        );
        assert_eq!(
            round_float_literal(PrimTy::F32, "1.000000059604644775390626", false),
            Some(FloatLiteralValue::F32(f32::from_bits(1.0f32.to_bits() + 1))),
            "direct f32 parsing retains the decimal value above the midpoint"
        );
        assert_eq!(
            round_float_literal(
                PrimTy::F64,
                "1.00000000000000011102230246251565404236316680908203125",
                false
            ),
            Some(FloatLiteralValue::F64(1.0)),
            "the exact f64 midpoint rounds to the even lower significand"
        );
        assert_eq!(
            round_float_literal(PrimTy::F32, "1.25", true),
            Some(FloatLiteralValue::F32(-1.25)),
            "the direct negative sign is retained"
        );
        assert_eq!(
            round_float_literal(PrimTy::F32, "1e-50", false),
            Some(FloatLiteralValue::F32(0.0)),
            "a nonzero value may round to positive zero"
        );
        assert_eq!(
            round_float_literal(PrimTy::F64, "1e-400", false),
            Some(FloatLiteralValue::F64(0.0)),
            "a nonzero value may round to positive zero"
        );
        assert!(
            !round_float_literal(PrimTy::F32, "3.4028236e38", false)
                .expect("valid F32 decimal parses")
                .is_finite(),
            "rounding a finite decimal to infinity is diagnosed"
        );
        assert!(
            !round_float_literal(PrimTy::F64, "1.7976931348623159e308", false)
                .expect("valid F64 decimal parses")
                .is_finite(),
            "rounding a finite decimal to infinity is diagnosed"
        );
    }
}
