//! Blueprint row N14b (carrick#3387, #3396): the match key of a consumer call
//! is decided by the base the ROW states, never by how the base is spelled.
//!
//! The deleted rule read the spelling: a leading `${x}` that was not
//! SCREAMING_SNAKE (and carried no `process.env.` text) was stripped, and one
//! that looked like an env read was kept. Each case below spells the base the
//! opposite way to what its stated kind says, so only a reader of the stated
//! kind passes.

use carrick::call_base::{CallBaseKind, CallBaseResolution};
use carrick::config::Config;
use carrick::url_normalizer::UrlNormalizer;

fn base(written: &str, kind: CallBaseKind) -> CallBaseResolution {
    CallBaseResolution {
        written: written.to_string(),
        kind,
        env_var: None,
        fallback: None,
        fallback_is_loopback: false,
        declared_optional: None,
    }
}

#[test]
fn the_match_key_follows_the_stated_base_kind_and_not_its_spelling() {
    let normalizer = UrlNormalizer::new(&Config::default());

    // Spelled like an env read, stated injected: stripped to the route.
    let injected = base("${PAYMENTS_URL}", CallBaseKind::Injected);
    assert_eq!(
        normalizer.consumer_call_path("${PAYMENTS_URL}/charges", Some(&injected)),
        "/charges"
    );

    // Spelled like a property, stated env: kept verbatim.
    let env = base("${gatewayUrl}", CallBaseKind::Env);
    assert_eq!(
        normalizer.consumer_call_path("${gatewayUrl}/orders", Some(&env)),
        "${gatewayUrl}/orders"
    );

    // A row that states no base keeps its target: nothing says it is injected.
    assert_eq!(
        normalizer.consumer_call_path("${clientBase}/orders", None),
        "${clientBase}/orders"
    );
}
