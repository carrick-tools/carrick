//! The one list of reasons a site the fact layer considered was not read
//! (carrick#3688 U1). Every stop vocabulary spells through [`Stop`]: the
//! discovery walk's stops, a lead's reason, a fact file's followed state, a
//! route chain's hole, an unfollowed member call's reason, and
//! `possible_calls[].unread.reason` on the index blob.
//!
//! [`HoleReason`] and [`InstanceHole`] stay as the internal causes the guards
//! and the instance walk match on. They have no spelling of their own: each
//! converts to a [`Stop`] by an exhaustive match and serialises through it.
//!
//! The phrase a person reads for each word is
//! `npm/carrick/src/unread-reasons.json` (one entry per word, checked by a
//! test here).

use serde::{Deserialize, Serialize};

use super::decided_sites::InstanceHole;
use super::guards::HoleReason;

/// Why a site was not read. One word per variant, spelled in `snake_case`.
///
/// Read leniently (carrick#3989 S0): a word this build does not know, which a
/// later release wrote on a sibling's blob, reads as [`Stop::Unknown`] holding
/// the raw word, and is written back as it was read. A reason word is never
/// an input to matching, findings or the PR result, so a word this build
/// cannot name is no reason to refuse the whole blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stop {
    /// The callee is an in-repo value that is no definition, and the resolver read none of its values.
    ValueCallee,
    /// The call or value is declared in a file of no service of the run.
    OutsideService,
    /// The declaring package has network exports and this call matches none.
    PackageUnmatched,
    /// The store has no answer for the declaring package yet.
    UnansweredPackage,
    /// A definition the walk reached has no body this pass read.
    NoBody,
    /// An interface member with no implementation answer.
    InterfaceMember,
    /// The callee's values reach different labelled exports, or one reaches none.
    SeveralValues,
    /// The callee holds more values than the walk follows.
    ValuesOverCap,
    /// Nothing binds the callee's value.
    ValueNeverBound,
    /// Every value of the callee is a function written at a caller.
    InjectedCallee,
    /// The implementations reach different labelled exports, or one reaches none.
    SeveralImplementations,
    /// The compiler engine found more implementations than it lists.
    ImplementationsOverCap,
    /// The walk ended with no site but passed a call it did not read.
    MissedInside,
    /// Discovery's labelled call could not be placed in the summaries' inputs.
    Unplaced,
    /// The slot keeps a parameter up to a function nothing calls.
    NameNeverBound,
    /// The slot or receiver is a parameter of a callback this code does not call.
    CallbackParameter,
    /// Branches settle at one call with different operations.
    SeveralOperations,
    /// A labelled export with a network role and no op.
    NoOpClaim,
    /// An answer fell below the band that states a row.
    BelowBand,
    /// The site's question was not answered on this scan.
    Unanswered,
    /// A route answer names no request method.
    NoMethod,
    /// A route answer whose router is unknown.
    OwnerUnknown,
    /// A row resting only on a label slot, with no reader agreeing.
    Uncorroborated,
    /// A route's instance is handed to a call no verified edge reads.
    MountUnverified,
    /// The function holding the receiver is called by nothing and handed to no library.
    EntryPoint,
    /// A library call runs the function and no scope label applies.
    RunsByLibrary,
    /// The function sits under a key of an options object handed to a call.
    CallbackOption,
    /// A value the instance walk does not read (a field, a call's value, an unlabelled maker, a conditional).
    Unread,
    /// The binding is written again, or declared twice.
    Reassigned,
    /// An expression form the resolver does not read (computed key, call result, this.x, spread, await).
    UnreadExpression,
    /// A value imported from a module of this service.
    InRepoValueImport,
    /// An import no module the scan reads resolves.
    UnresolvedImport,
    /// The function is handed to a function this repo defines.
    CallbackIntoOwnFunction,
    /// The function is handed to a call whose callee is unread.
    CalleeUnread,
    /// A call that names an operation calls an own function.
    OwnFunction,
    /// A named function nothing calls, hands on or exports.
    NoCallerNotExported,
    /// A function held in a local, returned or assigned.
    DetachedUnplaced,
    /// The caller passes nothing the resolver reads there.
    CallerArgMissing,
    /// Over a cap (more caller paths, or a query over its step budget).
    FanOutCap,
    /// The chain reaches a function it already passed.
    /// Older blobs spelled the route side of this `cycle`.
    Recursion,
    /// No compiler row answers the position.
    RowsMissing,
    /// The file's bytes differ from the ones its rows were read from.
    RowsStale,
    /// The walk read no rows for the file.
    NotInProgram,
    /// The type checker threw at the position.
    CheckerThrew,
    /// The compiler names no declaration, or types it as the error type.
    UnresolvedName,
    /// The receiver or value is typed any with no declaration.
    UntypedReceiver,
    /// A value reads as more than one written path.
    SeveralPaths,
    /// A procedure call's first key is declared outside the repository's
    /// source (a built declaration file, a package, another repository).
    ProcedureRouterOutsideRepo,
    /// A procedure call's first key is declared at more than one place.
    ProcedureRouterAmbiguous,
    /// A procedure call's key path holds a member the code does not name.
    ProcedurePathUnread,
    /// A parameter whose callers fill it, read by a reader that does not read through callers.
    ParameterUnread,
    /// A message name the code builds over an environment read: the run decides it.
    EnvValue,
    /// A procedure's adapter has no prefix and no host call that mounts it.
    ProcedureUnmounted,
    /// No adapter reaches the router that holds a procedure.
    ProcedureNoAdapter,
    /// A word a later release added: read from a blob, never written by
    /// this build's own passes, and in no [`Stop::ALL`]. A reader says the
    /// raw word, with no phrase.
    Unknown(UnknownWord),
}

/// The raw word of a [`Stop::Unknown`]. Only the deserialiser makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnknownWord(&'static str);

impl UnknownWord {
    /// The most distinct unknown words one process keeps, and the longest.
    const MAX_WORDS: usize = 256;
    const MAX_CHARS: usize = 64;
    /// What a word past either cap reads as.
    const OVER_CAP: &'static str = "unknown_reason";

    /// The word, kept once per process so [`Stop`] stays `Copy`. A blob is
    /// a sibling's, so the words it can add are bounded by the caps.
    fn intern(word: &str) -> UnknownWord {
        static WORDS: std::sync::Mutex<std::collections::BTreeSet<&'static str>> =
            std::sync::Mutex::new(std::collections::BTreeSet::new());
        let mut words = WORDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(kept) = words.get(word) {
            return UnknownWord(kept);
        }
        if word.chars().count() > Self::MAX_CHARS || words.len() >= Self::MAX_WORDS {
            return UnknownWord(Self::OVER_CAP);
        }
        let kept: &'static str = Box::leak(word.to_string().into_boxed_str());
        words.insert(kept);
        UnknownWord(kept)
    }
}

impl Stop {
    /// Every word, in the order of the enum.
    pub const ALL: [Stop; 54] = [
        Stop::ValueCallee,
        Stop::OutsideService,
        Stop::PackageUnmatched,
        Stop::UnansweredPackage,
        Stop::NoBody,
        Stop::InterfaceMember,
        Stop::SeveralValues,
        Stop::ValuesOverCap,
        Stop::ValueNeverBound,
        Stop::InjectedCallee,
        Stop::SeveralImplementations,
        Stop::ImplementationsOverCap,
        Stop::MissedInside,
        Stop::Unplaced,
        Stop::NameNeverBound,
        Stop::CallbackParameter,
        Stop::SeveralOperations,
        Stop::NoOpClaim,
        Stop::BelowBand,
        Stop::Unanswered,
        Stop::NoMethod,
        Stop::OwnerUnknown,
        Stop::Uncorroborated,
        Stop::MountUnverified,
        Stop::EntryPoint,
        Stop::RunsByLibrary,
        Stop::CallbackOption,
        Stop::Unread,
        Stop::Reassigned,
        Stop::UnreadExpression,
        Stop::InRepoValueImport,
        Stop::UnresolvedImport,
        Stop::CallbackIntoOwnFunction,
        Stop::CalleeUnread,
        Stop::OwnFunction,
        Stop::NoCallerNotExported,
        Stop::DetachedUnplaced,
        Stop::CallerArgMissing,
        Stop::FanOutCap,
        Stop::Recursion,
        Stop::RowsMissing,
        Stop::RowsStale,
        Stop::NotInProgram,
        Stop::CheckerThrew,
        Stop::UnresolvedName,
        Stop::UntypedReceiver,
        Stop::SeveralPaths,
        Stop::ProcedureRouterOutsideRepo,
        Stop::ProcedureRouterAmbiguous,
        Stop::ProcedurePathUnread,
        Stop::ParameterUnread,
        Stop::EnvValue,
        Stop::ProcedureUnmounted,
        Stop::ProcedureNoAdapter,
    ];

    /// The word as it is written on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Stop::ValueCallee => "value_callee",
            Stop::OutsideService => "outside_service",
            Stop::PackageUnmatched => "package_unmatched",
            Stop::UnansweredPackage => "unanswered_package",
            Stop::NoBody => "no_body",
            Stop::InterfaceMember => "interface_member",
            Stop::SeveralValues => "several_values",
            Stop::ValuesOverCap => "values_over_cap",
            Stop::ValueNeverBound => "value_never_bound",
            Stop::InjectedCallee => "injected_callee",
            Stop::SeveralImplementations => "several_implementations",
            Stop::ImplementationsOverCap => "implementations_over_cap",
            Stop::MissedInside => "missed_inside",
            Stop::Unplaced => "unplaced",
            Stop::NameNeverBound => "name_never_bound",
            Stop::CallbackParameter => "callback_parameter",
            Stop::SeveralOperations => "several_operations",
            Stop::NoOpClaim => "no_op_claim",
            Stop::BelowBand => "below_band",
            Stop::Unanswered => "unanswered",
            Stop::NoMethod => "no_method",
            Stop::OwnerUnknown => "owner_unknown",
            Stop::Uncorroborated => "uncorroborated",
            Stop::MountUnverified => "mount_unverified",
            Stop::EntryPoint => "entry_point",
            Stop::RunsByLibrary => "runs_by_library",
            Stop::CallbackOption => "callback_option",
            Stop::Unread => "unread",
            Stop::Reassigned => "reassigned",
            Stop::UnreadExpression => "unread_expression",
            Stop::InRepoValueImport => "in_repo_value_import",
            Stop::UnresolvedImport => "unresolved_import",
            Stop::CallbackIntoOwnFunction => "callback_into_own_function",
            Stop::CalleeUnread => "callee_unread",
            Stop::OwnFunction => "own_function",
            Stop::NoCallerNotExported => "no_caller_not_exported",
            Stop::DetachedUnplaced => "detached_unplaced",
            Stop::CallerArgMissing => "caller_arg_missing",
            Stop::FanOutCap => "fan_out_cap",
            Stop::Recursion => "recursion",
            Stop::RowsMissing => "rows_missing",
            Stop::RowsStale => "rows_stale",
            Stop::NotInProgram => "not_in_program",
            Stop::CheckerThrew => "checker_threw",
            Stop::UnresolvedName => "unresolved_name",
            Stop::UntypedReceiver => "untyped_receiver",
            Stop::SeveralPaths => "several_paths",
            Stop::ProcedureRouterOutsideRepo => "procedure_router_outside_repo",
            Stop::ProcedureRouterAmbiguous => "procedure_router_ambiguous",
            Stop::ProcedurePathUnread => "procedure_path_unread",
            Stop::ParameterUnread => "parameter_unread",
            Stop::EnvValue => "env_value",
            Stop::ProcedureUnmounted => "procedure_unmounted",
            Stop::ProcedureNoAdapter => "procedure_no_adapter",
            Stop::Unknown(UnknownWord(word)) => word,
        }
    }

    /// The stop a wire word names; `None` for a word that is no stop. Never
    /// [`Stop::Unknown`]: only a blob's read keeps a word this build does
    /// not know.
    pub fn parse(word: &str) -> Option<Stop> {
        Stop::ALL.into_iter().find(|stop| stop.as_str() == word)
    }

    /// What a reader says for this stop: the one phrase table every surface
    /// reads (R7, `npm/carrick/src/unread-reasons.json`). The CLI renders
    /// the phrase and sends it as text, so the npm port never opens the table.
    pub fn phrase(self) -> &'static str {
        static TABLE: std::sync::OnceLock<std::collections::BTreeMap<String, String>> =
            std::sync::OnceLock::new();
        TABLE
            .get_or_init(|| serde_json::from_str(PHRASES).unwrap_or_default())
            .get(self.as_str())
            .map_or(self.as_str(), String::as_str)
    }
}

/// The phrase table every surface reads (R7).
const PHRASES: &str = include_str!("../../npm/carrick/src/unread-reasons.json");

impl From<HoleReason> for Stop {
    fn from(reason: HoleReason) -> Stop {
        match reason {
            HoleReason::Reassigned => Stop::Reassigned,
            HoleReason::UnreadExpression => Stop::UnreadExpression,
            HoleReason::InRepoValueImport => Stop::InRepoValueImport,
            HoleReason::UnresolvedImport => Stop::UnresolvedImport,
            HoleReason::CallbackIntoOwnFunction => Stop::CallbackIntoOwnFunction,
            HoleReason::CalleeUnread => Stop::CalleeUnread,
            HoleReason::OwnFunction => Stop::OwnFunction,
            HoleReason::NoCallerNotExported => Stop::NoCallerNotExported,
            HoleReason::NameNeverBound => Stop::NameNeverBound,
            HoleReason::CallbackParameter => Stop::CallbackParameter,
            HoleReason::DetachedUnplaced => Stop::DetachedUnplaced,
            HoleReason::OutsideService => Stop::OutsideService,
            HoleReason::CallerArgMissing => Stop::CallerArgMissing,
            HoleReason::FanOutCap => Stop::FanOutCap,
            HoleReason::Recursion => Stop::Recursion,
            HoleReason::RowsMissing => Stop::RowsMissing,
            HoleReason::RowsStale => Stop::RowsStale,
            HoleReason::NotInProgram => Stop::NotInProgram,
            HoleReason::CheckerThrew => Stop::CheckerThrew,
            HoleReason::UnresolvedName => Stop::UnresolvedName,
            HoleReason::UntypedReceiver => Stop::UntypedReceiver,
            HoleReason::SeveralPaths => Stop::SeveralPaths,
            HoleReason::ParameterUnread => Stop::ParameterUnread,
        }
    }
}

impl From<InstanceHole> for Stop {
    fn from(reason: InstanceHole) -> Stop {
        match reason {
            InstanceHole::EntryPoint => Stop::EntryPoint,
            InstanceHole::CallbackParameter => Stop::CallbackParameter,
            InstanceHole::UntypedReceiver => Stop::UntypedReceiver,
            InstanceHole::OutsideService => Stop::OutsideService,
            InstanceHole::RunsByLibrary => Stop::RunsByLibrary,
            InstanceHole::CallbackOption => Stop::CallbackOption,
            InstanceHole::Unread => Stop::Unread,
        }
    }
}

impl Serialize for Stop {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Stop {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Stop, D::Error> {
        let word = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Ok(match word.as_ref() {
            // Older blobs spelled the route side of a recursion `cycle`.
            "cycle" => Stop::Recursion,
            known => {
                Stop::parse(known).unwrap_or_else(|| Stop::Unknown(UnknownWord::intern(known)))
            }
        })
    }
}

impl Serialize for HoleReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Stop::from(*self).serialize(serializer)
    }
}

impl Serialize for InstanceHole {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Stop::from(*self).serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stop_word_round_trips_and_has_a_phrase() {
        let phrases: std::collections::BTreeMap<String, String> =
            serde_json::from_str(PHRASES).unwrap();
        for stop in Stop::ALL {
            let word = stop.as_str();
            assert_eq!(serde_json::to_value(stop).unwrap().as_str(), Some(word));
            assert_eq!(serde_json::from_value::<Stop>(word.into()).unwrap(), stop);
            assert_eq!(Stop::parse(word), Some(stop));
            assert!(
                phrases
                    .get(word)
                    .is_some_and(|phrase| !phrase.trim().is_empty()),
                "no phrase for `{word}` in unread-reasons.json"
            );
        }
        let words: std::collections::BTreeSet<&str> =
            Stop::ALL.iter().map(|stop| stop.as_str()).collect();
        assert_eq!(words.len(), Stop::ALL.len(), "a word is spelled twice");
        for key in phrases.keys() {
            assert!(words.contains(key.as_str()), "`{key}` is no stop word");
        }
    }

    /// Every word a hole serialised to before the stop vocabularies merged.
    #[test]
    fn hole_and_instance_words_keep_their_spelling() {
        use HoleReason as H;
        for (reason, word) in [
            (H::Reassigned, "reassigned"),
            (H::UnreadExpression, "unread_expression"),
            (H::InRepoValueImport, "in_repo_value_import"),
            (H::UnresolvedImport, "unresolved_import"),
            (H::CallbackIntoOwnFunction, "callback_into_own_function"),
            (H::CalleeUnread, "callee_unread"),
            (H::OwnFunction, "own_function"),
            (H::NoCallerNotExported, "no_caller_not_exported"),
            (H::NameNeverBound, "name_never_bound"),
            (H::CallbackParameter, "callback_parameter"),
            (H::DetachedUnplaced, "detached_unplaced"),
            (H::OutsideService, "outside_service"),
            (H::CallerArgMissing, "caller_arg_missing"),
            (H::FanOutCap, "fan_out_cap"),
            (H::Recursion, "recursion"),
            (H::RowsMissing, "rows_missing"),
            (H::RowsStale, "rows_stale"),
            (H::NotInProgram, "not_in_program"),
            (H::CheckerThrew, "checker_threw"),
            (H::UnresolvedName, "unresolved_name"),
            (H::UntypedReceiver, "untyped_receiver"),
            (H::SeveralPaths, "several_paths"),
            (H::ParameterUnread, "parameter_unread"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), word);
            assert_eq!(Stop::from(reason).as_str(), word);
        }
        use InstanceHole as I;
        for (reason, word) in [
            (I::EntryPoint, "entry_point"),
            (I::CallbackParameter, "callback_parameter"),
            (I::UntypedReceiver, "untyped_receiver"),
            (I::OutsideService, "outside_service"),
            (I::RunsByLibrary, "runs_by_library"),
            (I::CallbackOption, "callback_option"),
            (I::Unread, "unread"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), word);
            assert_eq!(Stop::from(reason).as_str(), word);
        }
    }

    /// carrick#3989 S0: a word a later release added reads, keeps its
    /// spelling, is no stop this build names and has no phrase.
    #[test]
    fn a_word_from_a_later_release_reads_as_its_raw_word() {
        let word = "a_word_from_a_later_release";
        let stop = serde_json::from_value::<Stop>(word.into()).unwrap();
        assert!(matches!(stop, Stop::Unknown(_)));
        assert_eq!(stop.as_str(), word);
        assert_eq!(stop.phrase(), word);
        assert_eq!(serde_json::to_value(stop).unwrap(), word);
        assert_eq!(Stop::parse(word), None);
        assert!(!Stop::ALL.contains(&stop));
        let long = "w".repeat(UnknownWord::MAX_CHARS + 1);
        assert_eq!(
            serde_json::from_value::<Stop>(long.into())
                .unwrap()
                .as_str(),
            UnknownWord::OVER_CAP
        );
    }

    /// An older blob spelled a recursion `cycle`.
    #[test]
    fn an_older_cycle_reads_as_recursion() {
        assert_eq!(
            serde_json::from_value::<Stop>("cycle".into()).unwrap(),
            Stop::Recursion
        );
    }
}
