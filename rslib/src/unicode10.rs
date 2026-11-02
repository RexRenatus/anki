// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

//! Frozen Unicode 10.0.0 general-category predicates.
//!
//! These replace unic-ucd-category 0.9.0, whose unic crates are unmaintained
//! (RUSTSEC-2025-0075, RUSTSEC-2025-0080, RUSTSEC-2025-0081, RUSTSEC-2025-0094
//! and RUSTSEC-2025-0098). They keep that crate's Unicode 10.0.0 data, so
//! filename sanitising and typed-answer rendering behave as before.
#![cfg_attr(not(test), allow(dead_code))]

use std::cmp::Ordering;

/// Inclusive ranges of the scalar values assigned in Unicode 10.0.0 (every
/// general category but Cn).
const ASSIGNED: &[(u32, u32)] = &[];

/// Inclusive ranges of the Unicode 10.0.0 marks (Mn, Mc and Me).
const MARK: &[(u32, u32)] = &[];

fn in_ranges(ranges: &[(u32, u32)], c: char) -> bool {
    let cp = c as u32;
    ranges
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                Ordering::Less
            } else if lo > cp {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// True if `c` was unassigned (general category Cn) in Unicode 10.0.0.
pub(crate) fn is_unassigned(c: char) -> bool {
    !in_ranges(ASSIGNED, c)
}

/// True if `c` was a mark (Mn, Mc or Me) in Unicode 10.0.0.
pub(crate) fn is_mark(c: char) -> bool {
    in_ranges(MARK, c)
}

#[cfg(test)]
mod test {
    use unic_ucd_category::GeneralCategory;

    use super::*;

    #[test]
    fn unicode10_equals_unic_on_every_scalar_value() {
        let mut examined = 0u32;
        let mut assigned = 0u32;
        let mut mark = 0u32;
        let mut unassigned_diff = 0u32;
        let mut mark_diff = 0u32;
        for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
            examined += 1;
            let category = GeneralCategory::of(c);
            let unic_unassigned = category == GeneralCategory::Unassigned;
            let unic_mark = category.is_mark();
            assigned += u32::from(!unic_unassigned);
            mark += u32::from(unic_mark);
            unassigned_diff += u32::from(is_unassigned(c) != unic_unassigned);
            mark_diff += u32::from(is_mark(c) != unic_mark);
        }
        println!(
            "unicode10 examined={examined} assigned={assigned} mark={mark} \
             unassigned_diff={unassigned_diff} mark_diff={mark_diff}"
        );
        assert_eq!(examined, 1_112_064);
        assert_eq!(unassigned_diff, 0, "is_unassigned differs from unic");
        assert_eq!(mark_diff, 0, "is_mark differs from unic");
    }
}
