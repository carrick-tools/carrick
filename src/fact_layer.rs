//! The fact layer's switches, listed once.
//!
//! The cloud runner turns the fact layer on by setting each of these to `1`
//! (carrick-cloud `runner/image/src/proc.ts`). A laptop scan turns on the
//! same list ([`crate::local_mode`]'s scan command), so an edited file is
//! re-read with the detection the hosted rows were built with. This list is
//! the scanner's one copy: add a switch here and every local scan sets it.
//! The runner's copy lives in carrick-cloud and is matched by hand.

use std::ffi::OsString;

/// Every switch the fact layer runs under. Site questions and position rows
/// come first: the other three read what those two produce.
pub const SWITCHES: [&str; 5] = [
    crate::site_questions::SWITCH,
    crate::position_rows::POSITION_ROWS_ENV,
    crate::request_summary::discovery::RECEIVER_TRACE_SWITCH,
    crate::request_summary::fact_files::SWITCH,
    crate::site_payloads::SWITCH,
];

/// The switches a local scan sets to `1`: every one in [`SWITCHES`] that the
/// caller's environment (`read`) leaves unset. A value the caller set, `0`
/// included, is passed through untouched, so a comparison can turn one off.
pub fn local_defaults(read: impl Fn(&str) -> Option<OsString>) -> Vec<&'static str> {
    SWITCHES
        .into_iter()
        .filter(|name| read(name).is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_switch_is_turned_on_and_a_set_one_is_left_alone() {
        let set = |name: &str| {
            (name == crate::request_summary::fact_files::SWITCH).then(|| OsString::from("0"))
        };
        let on = local_defaults(set);
        assert!(!on.contains(&crate::request_summary::fact_files::SWITCH));
        assert_eq!(on.len(), SWITCHES.len() - 1);
        assert_eq!(local_defaults(|_| None), SWITCHES.to_vec());
    }
}
