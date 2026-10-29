//! Standalone validation of already-scanned command-line lint options.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;

use sai_diagnostics::{
    LINT_REGISTRY, LintNameLookup, OverrideLintLevel, Severity, lookup_lint_name,
};

/// An original argument and its one-based position, excluding the executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintOptionArgument {
    pub index: usize,
    pub argument: OsString,
}

/// An option occurrence supplied by a command-line scanner.
///
/// For `--warn=CODE`, `option` and `value` both identify the original token;
/// `name` contains only `CODE`. For `--warn CODE`, they identify separate tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintOptionOccurrence {
    pub level: OverrideLintLevel,
    pub name: OsString,
    pub option: LintOptionArgument,
    pub value: LintOptionArgument,
}

/// A validation error. Its occurrence preserves both original argv locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintOptionError {
    pub occurrence: LintOptionOccurrence,
    pub kind: LintOptionErrorKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LintOptionErrorKind {
    UnknownLint {
        name: String,
        candidates: Vec<&'static str>,
        case_correction: Option<&'static str>,
    },
    NotALint {
        name: String,
        severity: Severity,
    },
    ConflictingOptions {
        code: &'static str,
        first: LintOptionOccurrence,
    },
    InvalidEncoding,
}

/// The available registry does not provide lifecycle status for every code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeprecationValidation {
    Unsupported,
}

/// Settings contain only recognized lint codes without conflicting levels.
///
/// Deprecation and replacement-code handling are not validated. A caller must
/// not treat this result as complete lifecycle validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintOptionsValidation {
    pub settings: BTreeMap<&'static str, OverrideLintLevel>,
    pub errors: Vec<LintOptionError>,
    pub deprecation: DeprecationValidation,
}

/// Validates occurrences in supplied order without parsing argv or emitting diagnostics.
pub fn validate_lint_options(occurrences: &[LintOptionOccurrence]) -> LintOptionsValidation {
    let mut first_by_code: BTreeMap<&'static str, &LintOptionOccurrence> = BTreeMap::new();
    let mut settings = BTreeMap::new();
    let mut errors = Vec::new();

    for occurrence in occurrences {
        let Some(name) = occurrence.name.to_str() else {
            errors.push(LintOptionError {
                occurrence: occurrence.clone(),
                kind: LintOptionErrorKind::InvalidEncoding,
            });
            continue;
        };
        let kind = match lookup_lint_name(name) {
            LintNameLookup::Lint(entry) => {
                if let Some(first) = first_by_code.get(entry.code) {
                    if first.level != occurrence.level {
                        settings.remove(entry.code);
                        Some(LintOptionErrorKind::ConflictingOptions {
                            code: entry.code,
                            first: (*first).clone(),
                        })
                    } else {
                        None
                    }
                } else {
                    first_by_code.insert(entry.code, occurrence);
                    settings.insert(entry.code, occurrence.level);
                    None
                }
            }
            LintNameLookup::HardDiagnostic(severity) => Some(LintOptionErrorKind::NotALint {
                name: name.to_owned(),
                severity,
            }),
            LintNameLookup::Unknown => {
                let uppercase = name.to_uppercase();
                let case_correction = LINT_REGISTRY
                    .iter()
                    .find(|entry| entry.code == uppercase)
                    .map(|entry| entry.code);
                Some(LintOptionErrorKind::UnknownLint {
                    name: name.to_owned(),
                    candidates: lint_candidates(name, case_correction),
                    case_correction,
                })
            }
        };
        if let Some(kind) = kind {
            errors.push(LintOptionError {
                occurrence: occurrence.clone(),
                kind,
            });
        }
    }

    LintOptionsValidation {
        settings,
        errors,
        deprecation: DeprecationValidation::Unsupported,
    }
}

fn lint_candidates(name: &str, case_correction: Option<&str>) -> Vec<&'static str> {
    let length = name.chars().count();
    let max_length = LINT_REGISTRY
        .iter()
        .map(|entry| entry.code.chars().count())
        .max()
        .unwrap_or(0);
    if length > max_length.saturating_add(2) {
        return Vec::new();
    }
    let input = name.chars().collect::<Vec<_>>();
    let mut candidates = LINT_REGISTRY
        .iter()
        .filter(|entry| Some(entry.code) != case_correction)
        .filter_map(|entry| {
            let candidate = entry.code.chars().collect::<Vec<_>>();
            if length.abs_diff(candidate.len()) > 2 {
                return None;
            }
            let distance = damerau_levenshtein(&input, &candidate);
            (distance <= 2).then_some((distance, entry.code))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(distance, _)| *distance);
    candidates
        .into_iter()
        .take(10)
        .map(|(_, code)| code)
        .collect()
}

fn damerau_levenshtein(left: &[char], right: &[char]) -> usize {
    let maximum = left.len() + right.len();
    let mut distances = vec![vec![0; right.len() + 2]; left.len() + 2];
    distances[0][0] = maximum;
    for row in 0..=left.len() {
        distances[row + 1][0] = maximum;
        distances[row + 1][1] = row;
    }
    for column in 0..=right.len() {
        distances[0][column + 1] = maximum;
        distances[1][column + 1] = column;
    }
    let mut last_row = HashMap::new();
    for row in 1..=left.len() {
        let mut last_match_column = 0;
        for column in 1..=right.len() {
            let prior_row = last_row.get(&right[column - 1]).copied().unwrap_or(0);
            let prior_column = last_match_column;
            let substitution = if left[row - 1] == right[column - 1] {
                last_match_column = column;
                0
            } else {
                1
            };
            distances[row + 1][column + 1] = [
                distances[row][column] + substitution,
                distances[row + 1][column] + 1,
                distances[row][column + 1] + 1,
                distances[prior_row][prior_column]
                    + (row - prior_row - 1)
                    + 1
                    + (column - prior_column - 1),
            ]
            .into_iter()
            .min()
            .unwrap_or(maximum);
        }
        last_row.insert(left[row - 1], row);
    }
    distances[left.len() + 1][right.len() + 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(
        level: OverrideLintLevel,
        name: &str,
        option_index: usize,
        inline: bool,
    ) -> LintOptionOccurrence {
        let option_name = match level {
            OverrideLintLevel::Allow => "--allow",
            OverrideLintLevel::Warn => "--warn",
            OverrideLintLevel::Deny => "--deny",
        };
        let option = LintOptionArgument {
            index: option_index,
            argument: if inline {
                format!("{option_name}={name}").into()
            } else {
                option_name.into()
            },
        };
        LintOptionOccurrence {
            level,
            name: name.into(),
            value: if inline {
                option.clone()
            } else {
                LintOptionArgument {
                    index: option_index + 1,
                    argument: name.into(),
                }
            },
            option,
        }
    }

    #[test]
    fn valid_levels_and_same_level_duplicates_are_accepted() {
        let result = validate_lint_options(&[
            occurrence(OverrideLintLevel::Allow, "MISSING_DOCS", 3, false),
            occurrence(OverrideLintLevel::Allow, "MISSING_DOCS", 7, true),
            occurrence(OverrideLintLevel::Warn, "AMBIENT_IMPORT", 8, true),
        ]);
        assert_eq!(
            result.settings.get("MISSING_DOCS"),
            Some(&OverrideLintLevel::Allow)
        );
        assert_eq!(
            result.settings.get("AMBIENT_IMPORT"),
            Some(&OverrideLintLevel::Warn)
        );
        assert!(result.errors.is_empty());
        assert_eq!(result.deprecation, DeprecationValidation::Unsupported);
    }

    #[test]
    fn conflicts_never_choose_a_level_and_preserve_both_positions() {
        for (first_level, second_level) in [
            (OverrideLintLevel::Allow, OverrideLintLevel::Deny),
            (OverrideLintLevel::Deny, OverrideLintLevel::Allow),
        ] {
            let first = occurrence(first_level, "MISSING_DOCS", 3, false);
            let second = occurrence(second_level, "MISSING_DOCS", 8, true);
            let result = validate_lint_options(&[first.clone(), second.clone(), first.clone()]);
            assert!(!result.settings.contains_key("MISSING_DOCS"));
            assert_eq!(
                result.errors,
                vec![LintOptionError {
                    occurrence: second,
                    kind: LintOptionErrorKind::ConflictingOptions {
                        code: "MISSING_DOCS",
                        first
                    }
                }]
            );
        }
    }

    #[test]
    fn invalid_names_and_hard_codes_are_distinct_and_all_reported() {
        let result = validate_lint_options(&[
            occurrence(OverrideLintLevel::Warn, "UNUSED_VARIABLES", 2, false),
            occurrence(OverrideLintLevel::Allow, "SOURCE_READ_FAILED", 5, true),
            occurrence(OverrideLintLevel::Deny, "CACHE_CORRUPT", 6, false),
            occurrence(OverrideLintLevel::Warn, "MISSING_DOCS,UNUSED_ITEM", 9, true),
        ]);
        assert!(result.settings.is_empty());
        assert_eq!(result.errors.len(), 4);
        assert_eq!(result.errors[0].occurrence.value.index, 3);
        assert!(
            matches!(&result.errors[0].kind, LintOptionErrorKind::UnknownLint { name, candidates, .. } if name == "UNUSED_VARIABLES" && candidates.contains(&"UNUSED_VARIABLE"))
        );
        assert!(matches!(
            &result.errors[1].kind,
            LintOptionErrorKind::NotALint {
                severity: Severity::Error,
                ..
            }
        ));
        assert_eq!(
            result.errors[1].occurrence.value.argument,
            "--allow=SOURCE_READ_FAILED"
        );
        assert!(matches!(
            &result.errors[2].kind,
            LintOptionErrorKind::NotALint {
                severity: Severity::Warning,
                ..
            }
        ));
        assert!(matches!(
            &result.errors[3].kind,
            LintOptionErrorKind::UnknownLint { .. }
        ));
    }

    #[test]
    fn typo_candidates_use_fixed_distance_and_case_correction_is_separate() {
        let result = validate_lint_options(&[
            occurrence(OverrideLintLevel::Warn, "missing_docs", 1, false),
            occurrence(OverrideLintLevel::Warn, "UNUSED_VAIRABLE", 3, false),
        ]);
        assert!(
            matches!(&result.errors[0].kind, LintOptionErrorKind::UnknownLint { candidates, case_correction: Some("MISSING_DOCS"), .. } if !candidates.contains(&"MISSING_DOCS"))
        );
        assert!(
            matches!(&result.errors[1].kind, LintOptionErrorKind::UnknownLint { candidates, .. } if candidates.contains(&"UNUSED_VARIABLE"))
        );
        assert_eq!(damerau_levenshtein(&['C', 'A'], &['A', 'B', 'C']), 2);
        assert!(lint_candidates(&"x".repeat(100_000), None).is_empty());
    }

    #[test]
    fn unicode_uppercase_corrections_include_expansions() {
        for name in [
            "mıssıng_docs",
            "MıSSıNG_DOCS",
            "mißing_docs",
            "MiSsInG_DoCs",
        ] {
            let result =
                validate_lint_options(&[occurrence(OverrideLintLevel::Warn, name, 1, true)]);
            assert!(result.settings.is_empty());
            assert!(matches!(
                &result.errors[0].kind,
                LintOptionErrorKind::UnknownLint {
                    name: original,
                    candidates,
                    case_correction: Some("MISSING_DOCS"),
                } if original == name && !candidates.contains(&"MISSING_DOCS")
            ));
        }

        for name in ["mıssıng_docx", "cache_corrupt"] {
            let result =
                validate_lint_options(&[occurrence(OverrideLintLevel::Warn, name, 1, false)]);
            assert!(matches!(
                &result.errors[0].kind,
                LintOptionErrorKind::UnknownLint {
                    case_correction: None,
                    ..
                }
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_name_preserves_original_argument() {
        use std::os::unix::ffi::OsStringExt;

        let mut invalid = occurrence(OverrideLintLevel::Allow, "invalid", 4, false);
        invalid.name = OsString::from_vec(vec![0xff]);
        invalid.value.argument = invalid.name.clone();
        let result = validate_lint_options(&[invalid.clone()]);
        assert_eq!(
            result.errors,
            vec![LintOptionError {
                occurrence: invalid,
                kind: LintOptionErrorKind::InvalidEncoding
            }]
        );
    }

    #[test]
    fn empty_names_and_hard_info_codes_are_not_accepted() {
        let result = validate_lint_options(&[
            occurrence(OverrideLintLevel::Allow, "", 1, true),
            occurrence(OverrideLintLevel::Warn, "NOT_PRESENT_IN_BUILD", 2, true),
        ]);
        assert!(result.settings.is_empty());
        assert!(
            matches!(&result.errors[0].kind, LintOptionErrorKind::UnknownLint { name, candidates, .. } if name.is_empty() && candidates.is_empty())
        );
        assert!(matches!(
            &result.errors[1].kind,
            LintOptionErrorKind::NotALint {
                severity: Severity::Info,
                ..
            }
        ));
    }

    #[test]
    fn candidate_order_is_distance_then_registry_order() {
        let candidates = lint_candidates("UNUSED_ITM", None);
        assert_eq!(candidates.first(), Some(&"UNUSED_ITEM"));
        assert!(candidates.len() <= 10);
        let input = "UNUSED_ITM".chars().collect::<Vec<_>>();
        let ranks = candidates
            .iter()
            .map(|code| {
                let order = LINT_REGISTRY
                    .iter()
                    .position(|entry| entry.code == *code)
                    .unwrap();
                (
                    damerau_levenshtein(&input, &code.chars().collect::<Vec<_>>()),
                    order,
                )
            })
            .collect::<Vec<_>>();
        assert!(ranks.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(ranks.iter().all(|(distance, _)| *distance <= 2));
    }
}
