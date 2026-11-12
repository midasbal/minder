//! The decision log: one NEP-297 event for every agent request that reaches the
//! policy decision, allowed or denied, carrying the request's inputs and the
//! verdict. It is the source a read-only control panel streams, and what an
//! auditor checks each decision against.
//!
//! Purely additive. It reads nothing from storage, writes nothing, and changes
//! no decision: the three request methods compute their decision exactly as
//! before and then call [`log`] with it, an allow before anything is reserved
//! or dispatched, a deny before the panic that reverts the request. A deny
//! reverts, but the logs of a failed receipt are kept in its execution outcome,
//! so the event of a denied request is readable there.
//!
//! Event, standard `minder`, version `1.0.0`, name `decision`:
//!
//! ```json
//! { "method": "request_evm_token_payment",
//!   "agent_key": "ed25519:..." | null,
//!   "asset": <AssetId>,           // the asset the agent asked to move
//!   "recipient": "bob.near" | "0x...",
//!   "amount": "1000",             // as the agent gave it, in the asset's units
//!   "decision": "allow" | "deny",
//!   "reason": null | "<DenyReason>",   // of the first denying leg
//!   "leg": null | "transfer" | "gas",  // the leg that denied
//!   "legs": [ { "leg": "transfer", "asset": <AssetId>, "amount": "1000",
//!               "decision": "allow" | "deny", "reason": null | "<DenyReason>" },
//!             { "leg": "gas", ... } ] }
//! ```
//!
//! `legs` lists the legs that were evaluated, in order. A native payment and a
//! local transfer have the one `transfer` leg, whose `amount` is what policy
//! counted (for a native EVM payment the outlay, the value plus the worst-case
//! fee). A token payment has a `transfer` leg (the token amount) and a `gas`
//! leg (the worst-case gas in wei against the native asset); the gas leg is
//! absent when the transfer leg already denied, since the request stops there.
//! `agent_key` is the key that signed the call, or null if the call did not come
//! from the contract's own account.
//!
//! Requests rejected before any policy is consulted (a malformed address, an
//! EVM payment with no EVM configuration, an EVM asset passed to the NEAR
//! entry point) revert without a decision event: they are input errors, not
//! verdicts.

use near_sdk::serde_json::{json, Value};
use near_sdk::{env, PublicKey};

use crate::types::{AssetId, Decision};

/// What the agent asked for.
pub(crate) struct Request<'a> {
    pub method: &'static str,
    pub agent_key: Option<&'a PublicKey>,
    pub asset: &'a AssetId,
    pub recipient: String,
    pub amount: u128,
}

/// One evaluated leg of a request and its decision.
pub(crate) struct Leg<'a> {
    pub leg: &'static str,
    pub asset: &'a AssetId,
    pub amount: u128,
    pub decision: Decision,
}

fn verdict(decision: Decision) -> (&'static str, Value) {
    match decision {
        Decision::Allow => ("allow", Value::Null),
        Decision::Deny(reason) => ("deny", json!(format!("{reason:?}"))),
    }
}

/// The event line. Pure, so the schema can be tested without an environment.
pub(crate) fn event_line(request: &Request, legs: &[Leg]) -> String {
    let denying = legs
        .iter()
        .find(|leg| matches!(leg.decision, Decision::Deny(_)));
    let (decision, reason, leg) = match denying {
        Some(leg) => {
            let (decision, reason) = verdict(leg.decision);
            (decision, reason, json!(leg.leg))
        }
        None => ("allow", Value::Null, Value::Null),
    };
    let legs: Vec<Value> = legs
        .iter()
        .map(|leg| {
            let (decision, reason) = verdict(leg.decision);
            json!({
                "leg": leg.leg,
                "asset": leg.asset,
                "amount": leg.amount.to_string(),
                "decision": decision,
                "reason": reason,
            })
        })
        .collect();
    let event = json!({
        "standard": "minder",
        "version": "1.0.0",
        "event": "decision",
        "data": [{
            "method": request.method,
            "agent_key": request.agent_key,
            "asset": request.asset,
            "recipient": request.recipient,
            "amount": request.amount.to_string(),
            "decision": decision,
            "reason": reason,
            "leg": leg,
            "legs": legs,
        }],
    });
    format!("EVENT_JSON:{event}")
}

/// Emits the decision of `request`. Call it with the decision already made and
/// before acting on it.
pub(crate) fn log(request: &Request, legs: &[Leg]) {
    env::log_str(&event_line(request, legs));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DenyReason;
    use near_sdk::serde_json;

    fn parse(line: &str) -> Value {
        let json = line.strip_prefix("EVENT_JSON:").expect("an event line");
        serde_json::from_str(json).expect("JSON")
    }

    fn request(asset: &AssetId) -> Request<'_> {
        Request {
            method: "request_evm_token_payment",
            agent_key: None,
            asset,
            recipient: "0x70997970c51812dc3a010c7d01b50e0d17dc79c8".to_string(),
            amount: 1000,
        }
    }

    #[test]
    fn an_allow_carries_the_inputs_and_every_leg() {
        let (token, native) = (
            AssetId::Evm20 {
                chain_id: 5,
                token: [7; 20],
            },
            AssetId::Evm(5),
        );
        let event = parse(&event_line(
            &request(&token),
            &[
                Leg {
                    leg: "transfer",
                    asset: &token,
                    amount: 1000,
                    decision: Decision::Allow,
                },
                Leg {
                    leg: "gas",
                    asset: &native,
                    amount: 200,
                    decision: Decision::Allow,
                },
            ],
        ));
        assert_eq!(event["standard"], "minder");
        assert_eq!(event["version"], "1.0.0");
        assert_eq!(event["event"], "decision");
        let data = &event["data"][0];
        assert_eq!(data["method"], "request_evm_token_payment");
        assert_eq!(data["agent_key"], Value::Null);
        assert_eq!(data["amount"], "1000");
        assert_eq!(data["decision"], "allow");
        assert_eq!(data["reason"], Value::Null);
        assert_eq!(data["leg"], Value::Null);
        assert_eq!(data["legs"].as_array().unwrap().len(), 2);
        assert_eq!(data["legs"][1]["leg"], "gas");
        assert_eq!(data["legs"][1]["amount"], "200");
        assert_eq!(data["legs"][1]["asset"], json!({ "Evm": 5 }));
    }

    #[test]
    fn a_deny_names_the_first_denying_leg_and_its_reason() {
        let (token, native) = (
            AssetId::Evm20 {
                chain_id: 5,
                token: [7; 20],
            },
            AssetId::Evm(5),
        );
        let event = parse(&event_line(
            &request(&token),
            &[
                Leg {
                    leg: "transfer",
                    asset: &token,
                    amount: 1000,
                    decision: Decision::Allow,
                },
                Leg {
                    leg: "gas",
                    asset: &native,
                    amount: 200,
                    decision: Decision::Deny(DenyReason::WindowLimitExceeded),
                },
            ],
        ));
        let data = &event["data"][0];
        assert_eq!(data["decision"], "deny");
        assert_eq!(data["reason"], "WindowLimitExceeded");
        assert_eq!(data["leg"], "gas");
        assert_eq!(data["legs"][0]["decision"], "allow");
        assert_eq!(data["legs"][1]["reason"], "WindowLimitExceeded");
    }
}
