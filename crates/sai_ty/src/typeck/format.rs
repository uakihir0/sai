//! F-string format-specifier normalization.

use sai_diagnostics::Code;
use sai_hir::expressions::FmtSpec;
use sai_intern::Interner;

/// The normalized value passed to `Format.format` for one interpolation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NormalizedFormatSpec {
    Plain,
    Debug,
    Hex,
    UpperHex,
    Bin,
    Oct,
    Exp,
    Prec(u16),
    Pad(u16),
    Left(u16),
    Zpad(u16),
}

/// The marker-trait category required by a normalized format specifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredFormatTrait {
    Format,
    FormatInt,
    FormatFloat,
    FormatPad,
}

/// The normalized specifier and its required marker-trait category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatSpecNormalization {
    pub spec: NormalizedFormatSpec,
    pub required_trait: RequiredFormatTrait,
}

/// A format-specifier name or argument error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatSpecErrorKind {
    UnknownName,
    Argument,
}

impl FormatSpecErrorKind {
    /// The diagnostic code associated with this normalization error.
    pub const fn diagnostic_code(self) -> Code {
        match self {
            Self::UnknownName => Code::UnknownFormatSpec,
            Self::Argument => Code::FormatSpecArgument,
        }
    }
}

/// Normalize an optional HIR format specifier without checking operand traits.
///
/// Unknown names map to `UNKNOWN_FORMAT_SPEC` even when an argument is
/// present. Known parameterized specifiers require an integer in `0..=65535`;
/// all other known specifiers forbid an argument.
pub fn normalize_format_spec(
    spec: Option<FmtSpec>,
    symbols: &Interner,
) -> Result<FormatSpecNormalization, FormatSpecErrorKind> {
    let Some(spec) = spec else {
        return Ok(FormatSpecNormalization {
            spec: NormalizedFormatSpec::Plain,
            required_trait: RequiredFormatTrait::Format,
        });
    };

    let name = symbols
        .resolve(spec.name)
        .map_err(|_| FormatSpecErrorKind::UnknownName)?;

    let (required_trait, needs_argument) = match name.as_ref() {
        "debug" => (RequiredFormatTrait::Format, false),
        "hex" | "upper_hex" | "bin" | "oct" => (RequiredFormatTrait::FormatInt, false),
        "exp" => (RequiredFormatTrait::FormatFloat, false),
        "prec" => (RequiredFormatTrait::FormatFloat, true),
        "pad" | "left" => (RequiredFormatTrait::FormatPad, true),
        "zpad" => (RequiredFormatTrait::FormatInt, true),
        _ => return Err(FormatSpecErrorKind::UnknownName),
    };

    let argument = match (needs_argument, spec.arg) {
        (true, Some(argument)) if argument <= u16::MAX.into() => Some(argument as u16),
        (false, None) => None,
        _ => return Err(FormatSpecErrorKind::Argument),
    };
    let normalized_spec = match (name.as_ref(), argument) {
        ("debug", None) => NormalizedFormatSpec::Debug,
        ("hex", None) => NormalizedFormatSpec::Hex,
        ("upper_hex", None) => NormalizedFormatSpec::UpperHex,
        ("bin", None) => NormalizedFormatSpec::Bin,
        ("oct", None) => NormalizedFormatSpec::Oct,
        ("exp", None) => NormalizedFormatSpec::Exp,
        ("prec", Some(argument)) => NormalizedFormatSpec::Prec(argument),
        ("pad", Some(argument)) => NormalizedFormatSpec::Pad(argument),
        ("left", Some(argument)) => NormalizedFormatSpec::Left(argument),
        ("zpad", Some(argument)) => NormalizedFormatSpec::Zpad(argument),
        _ => return Err(FormatSpecErrorKind::Argument),
    };

    Ok(FormatSpecNormalization {
        spec: normalized_spec,
        required_trait,
    })
}

#[cfg(test)]
mod tests {
    use sai_hir::{Span, SpanCtxt, expressions::FmtSpec};
    use sai_intern::{Interner, Symbol};
    use sai_vfs::{FileId, TextRange, TextSize};

    use super::{
        FormatSpecErrorKind, FormatSpecNormalization, NormalizedFormatSpec, RequiredFormatTrait,
        normalize_format_spec,
    };

    fn spec(symbols: &Interner, name: &str, arg: Option<u128>) -> FmtSpec {
        FmtSpec {
            name: symbols.intern(name).expect("specifier name interns"),
            arg,
            span: Span {
                file: FileId::new(0),
                range: TextRange::empty(TextSize::new(0)),
                ctxt: SpanCtxt::Root,
            },
        }
    }

    fn expected(
        spec: NormalizedFormatSpec,
        required_trait: RequiredFormatTrait,
    ) -> FormatSpecNormalization {
        FormatSpecNormalization {
            spec,
            required_trait,
        }
    }

    fn parameterized(name: &str, argument: u16) -> NormalizedFormatSpec {
        match name {
            "prec" => NormalizedFormatSpec::Prec(argument),
            "pad" => NormalizedFormatSpec::Pad(argument),
            "left" => NormalizedFormatSpec::Left(argument),
            "zpad" => NormalizedFormatSpec::Zpad(argument),
            _ => unreachable!("test case uses a parameterized specifier"),
        }
    }

    #[test]
    fn normalizes_every_known_format_specifier_and_trait_category() {
        let symbols = Interner::new();

        assert_eq!(
            normalize_format_spec(None, &symbols),
            Ok(expected(
                NormalizedFormatSpec::Plain,
                RequiredFormatTrait::Format
            ))
        );

        let cases = [
            (
                "debug",
                None,
                NormalizedFormatSpec::Debug,
                RequiredFormatTrait::Format,
            ),
            (
                "hex",
                None,
                NormalizedFormatSpec::Hex,
                RequiredFormatTrait::FormatInt,
            ),
            (
                "upper_hex",
                None,
                NormalizedFormatSpec::UpperHex,
                RequiredFormatTrait::FormatInt,
            ),
            (
                "bin",
                None,
                NormalizedFormatSpec::Bin,
                RequiredFormatTrait::FormatInt,
            ),
            (
                "oct",
                None,
                NormalizedFormatSpec::Oct,
                RequiredFormatTrait::FormatInt,
            ),
            (
                "exp",
                None,
                NormalizedFormatSpec::Exp,
                RequiredFormatTrait::FormatFloat,
            ),
            (
                "prec",
                Some(12),
                NormalizedFormatSpec::Prec(12),
                RequiredFormatTrait::FormatFloat,
            ),
            (
                "pad",
                Some(12),
                NormalizedFormatSpec::Pad(12),
                RequiredFormatTrait::FormatPad,
            ),
            (
                "left",
                Some(12),
                NormalizedFormatSpec::Left(12),
                RequiredFormatTrait::FormatPad,
            ),
            (
                "zpad",
                Some(12),
                NormalizedFormatSpec::Zpad(12),
                RequiredFormatTrait::FormatInt,
            ),
        ];

        for (name, argument, normalized, required_trait) in cases {
            assert_eq!(
                normalize_format_spec(Some(spec(&symbols, name, argument)), &symbols),
                Ok(expected(normalized, required_trait)),
                "specifier {name:?}"
            );
        }
    }

    #[test]
    fn accepts_zero_and_the_maximum_width_for_each_parameterized_specifier() {
        let symbols = Interner::new();
        let cases = ["prec", "pad", "left", "zpad"];

        for name in cases {
            for argument in [0, 65_535] {
                assert_eq!(
                    normalize_format_spec(Some(spec(&symbols, name, Some(argument))), &symbols),
                    Ok(expected(
                        parameterized(name, argument as u16),
                        match name {
                            "prec" => RequiredFormatTrait::FormatFloat,
                            "pad" | "left" => RequiredFormatTrait::FormatPad,
                            "zpad" => RequiredFormatTrait::FormatInt,
                            _ => unreachable!(),
                        }
                    )),
                    "specifier {name:?} with argument {argument}"
                );
            }
        }
    }

    #[test]
    fn rejects_a_missing_argument_for_each_parameterized_specifier() {
        let symbols = Interner::new();

        for name in ["prec", "pad", "left", "zpad"] {
            assert_eq!(
                normalize_format_spec(Some(spec(&symbols, name, None)), &symbols),
                Err(FormatSpecErrorKind::Argument),
                "specifier {name:?}"
            );
        }
    }

    #[test]
    fn rejects_an_argument_for_each_non_parameterized_specifier() {
        let symbols = Interner::new();

        for name in ["debug", "hex", "upper_hex", "bin", "oct", "exp"] {
            assert_eq!(
                normalize_format_spec(Some(spec(&symbols, name, Some(0))), &symbols),
                Err(FormatSpecErrorKind::Argument),
                "specifier {name:?}"
            );
        }
    }

    #[test]
    fn rejects_arguments_above_the_maximum_for_each_parameterized_specifier() {
        let symbols = Interner::new();

        for name in ["prec", "pad", "left", "zpad"] {
            assert_eq!(
                normalize_format_spec(Some(spec(&symbols, name, Some(65_536))), &symbols),
                Err(FormatSpecErrorKind::Argument),
                "specifier {name:?}"
            );
        }
    }

    #[test]
    fn maps_unknown_names_to_unknown_format_spec_even_when_an_argument_is_present() {
        let symbols = Interner::new();

        for argument in [None, Some(0)] {
            let error = normalize_format_spec(Some(spec(&symbols, "unknown", argument)), &symbols)
                .expect_err("unknown names are rejected");
            assert_eq!(error, FormatSpecErrorKind::UnknownName);
            assert_eq!(
                error.diagnostic_code(),
                sai_diagnostics::Code::UnknownFormatSpec
            );
        }
    }

    #[test]
    fn maps_an_unresolved_hir_name_symbol_to_unknown_format_spec() {
        let symbols = Interner::new();
        let unresolved = FmtSpec {
            name: Symbol::ERROR,
            arg: None,
            span: Span {
                file: FileId::new(0),
                range: TextRange::empty(TextSize::new(0)),
                ctxt: SpanCtxt::Root,
            },
        };

        assert_eq!(
            normalize_format_spec(Some(unresolved), &symbols),
            Err(FormatSpecErrorKind::UnknownName)
        );
    }

    #[test]
    fn maps_argument_errors_to_format_spec_argument() {
        let error = FormatSpecErrorKind::Argument;
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::FormatSpecArgument
        );
    }
}
