use tempo_agentic_price::{PricePair, chain_slug, parse_tick};

const WETH_BASE: &str = "0x4200000000000000000000000000000000000006";

fn pair() -> PricePair {
    PricePair::new("base", WETH_BASE)
}

// A payload captured from the live feed. Note the price arrives as a string, not
// a number: parsing it as a number only would drop every real quote.
#[test]
fn the_real_feed_payload_parses() {
    let payload = r#"{"address":"0x4200000000000000000000000000000000000006","chain":"base","price":"1591.6871308001703","timestamp":1782914872}"#;
    let tick = parse_tick(&pair(), payload).expect("real payload should parse");
    assert_eq!(tick.price_usd, 1591.6871308001703);
    assert_eq!(tick.published_at, 1782914872);
    assert_eq!(tick.pair, pair());
}

#[test]
fn price_is_accepted_as_a_number_too() {
    let payload =
        format!(r#"{{"address":"{WETH_BASE}","chain":"base","price":1800.5,"timestamp":42}}"#);
    let tick = parse_tick(&pair(), &payload).expect("numeric price should parse");
    assert_eq!(tick.price_usd, 1800.5);
}

#[test]
fn price_usd_is_accepted_as_an_alias() {
    let payload =
        format!(r#"{{"address":"{WETH_BASE}","chain":"base","price_usd":"12.5","timestamp":42}}"#);
    let tick = parse_tick(&pair(), &payload).expect("price_usd should parse");
    assert_eq!(tick.price_usd, 12.5);
}

#[test]
fn a_wrapped_payload_parses() {
    let payload = format!(
        r#"{{"data":{{"address":"{WETH_BASE}","chain":"base","price":99.25,"timestamp":7}}}}"#
    );
    let tick = parse_tick(&pair(), &payload).expect("nested payload should parse");
    assert_eq!(tick.price_usd, 99.25);
    assert_eq!(tick.published_at, 7);
}

// An EIP-55 checksummed address and its lowercase form name the same token, so
// the feed's casing must not decide whether a quote is usable.
#[test]
fn address_casing_does_not_matter() {
    let checksummed = "0x4200000000000000000000000000000000000006";
    let lowercase = PricePair::new("BASE", checksummed.to_ascii_lowercase());
    let payload =
        format!(r#"{{"address":"{checksummed}","chain":"base","price":1.0,"timestamp":1}}"#);
    assert!(parse_tick(&lowercase, &payload).is_some());
}

// A crossed subscription must not be priced. Without this the daemon would spend
// funds against whatever token the feed happened to send.
#[test]
fn a_frame_about_another_token_is_rejected() {
    let usdc = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    let payload = format!(r#"{{"address":"{usdc}","chain":"base","price":1.0,"timestamp":1}}"#);
    assert_eq!(parse_tick(&pair(), &payload), None);
}

#[test]
fn a_frame_about_another_chain_is_rejected() {
    let payload =
        format!(r#"{{"address":"{WETH_BASE}","chain":"arbitrum","price":1.0,"timestamp":1}}"#);
    assert_eq!(parse_tick(&pair(), &payload), None);
}

#[test]
fn a_frame_that_names_no_token_is_rejected() {
    assert_eq!(parse_tick(&pair(), r#"{"price":1.0,"timestamp":1}"#), None);
}

// Without a timestamp the staleness gate has nothing to check, and defaulting to
// "now" would disable it silently.
#[test]
fn a_frame_without_a_timestamp_is_rejected() {
    let payload = format!(r#"{{"address":"{WETH_BASE}","chain":"base","price":1.0}}"#);
    assert_eq!(parse_tick(&pair(), &payload), None);
}

#[test]
fn a_frame_without_a_price_is_rejected() {
    let payload = format!(r#"{{"address":"{WETH_BASE}","chain":"base","timestamp":1}}"#);
    assert_eq!(parse_tick(&pair(), &payload), None);
}

#[test]
fn an_unparseable_price_string_is_rejected() {
    let payload =
        format!(r#"{{"address":"{WETH_BASE}","chain":"base","price":"n/a","timestamp":1}}"#);
    assert_eq!(parse_tick(&pair(), &payload), None);
}

#[test]
fn heartbeats_and_malformed_frames_are_skipped_not_fatal() {
    assert_eq!(parse_tick(&pair(), "not json"), None);
    assert_eq!(parse_tick(&pair(), "{}"), None);
    assert_eq!(parse_tick(&pair(), r#"{"event":"ping"}"#), None);
}

// These are exactly the chains Config::validate accepts; the two lists have to
// move together or a level ends up with nothing pricing it.
#[test]
fn every_supported_chain_has_a_slug() {
    assert_eq!(chain_slug(1), Some("ethereum"));
    assert_eq!(chain_slug(8453), Some("base"));
    assert_eq!(chain_slug(42161), Some("arbitrum"));
}

#[test]
fn an_unsupported_chain_has_no_fallback_slug() {
    assert_eq!(chain_slug(137), None);
    assert_eq!(chain_slug(0), None);
}
