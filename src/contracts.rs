//! Authoritative Herdr contracts, extracted from the pinned upstream Git source.
//!
//! See `build_support/contracts.rs` for the extraction boundary. No schema types
//! are copied or maintained here. Field ordering, variants, serde defaults and
//! custom serializers come from Herdr's source (Apache-2.0, `vendor/herdr/LICENSE`).
#![allow(dead_code, unused_imports, private_interfaces)]
include!(concat!(env!("OUT_DIR"), "/herdr_contracts.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn full_api_preserves_methods_outside_neordrs_implementation() {
        // These cover independently shaped API branches that Neordr does not
        // implement; extraction must not silently reduce the schema to its usage.
        for (name, params) in [
            ("plugin.list", json!({})),
            ("server.reload_config", json!({})),
            (
                "events.subscribe",
                json!({"subscriptions": [{"type": "workspace.created"}]}),
            ),
            (
                "pane.selection.read",
                json!({"pane_id": "p", "anchor": {"row": 1,"col": 2}, "cursor": {"row": 3,"col": 4}}),
            ),
        ] {
            let request: api::Request = serde_json::from_value(json!({
                "id": "test", "method": name, "params": params
            }))
            .unwrap();
            assert_eq!(method_name(&request.method), Some(name));
            assert!(method_names().contains(&name));
        }
        assert!(method_names().len() > 100);
        assert!(
            serde_json::from_value::<api::Request>(json!({
                "id": "test", "method": "pane.focus", "params": {"pane_id": 42}
            }))
            .is_err()
        );
        let schema = serde_json::to_value(schemars::schema_for!(api::Method)).unwrap();
        assert!(schema["oneOf"].as_array().unwrap().len() > 100);
    }

    #[test]
    fn serde_defaults_and_custom_codecs_are_upstream_behavior() {
        let hello: endpoint::EndpointClientHello = serde_json::from_value(json!({
            "generation": 1, "cell_width_px": 8, "cell_height_px": 16,
            "surface_size": {"cols": 80, "rows": 24}, "pixel_mouse": false,
            "direct_graphics": false, "endpoint_keybindings": false,
            "mouse_capture": false
        }))
        .unwrap();
        assert!(hello.surface_active);
        assert!(!hello.surface_reuse);
        assert!(!hello.supports_required_codecs());
        assert_eq!(
            serde_json::from_value::<popup_size::PopupSize>(json!("80%")).unwrap(),
            popup_size::PopupSize::Percent(80)
        );
        assert!(serde_json::from_value::<popup_size::PopupSize>(json!("101%")).is_err());
        assert_eq!(
            serde_json::from_value::<wire::ClientShellCommandAction>(json!("FutureAction"))
                .unwrap(),
            wire::ClientShellCommandAction::Unknown
        );
    }

    #[test]
    fn stable_binary_discriminants_match_upstream_compatibility_fixtures() {
        // Fixed independently in upstream protocol/wire.rs compatibility tests.
        let config = bincode::config::standard();
        assert_eq!(
            bincode::serde::encode_to_vec(wire::ClientMessage::Detach, config).unwrap(),
            vec![4]
        );
        assert_eq!(
            bincode::serde::encode_to_vec(
                wire::ClientMessage::EndpointControl {
                    kind: String::new(),
                    data: String::new()
                },
                config
            )
            .unwrap(),
            vec![20, 0, 0]
        );
        assert_eq!(
            bincode::serde::encode_to_vec(
                wire::ServerMessage::EndpointControl {
                    kind: String::new(),
                    data: String::new()
                },
                config
            )
            .unwrap(),
            vec![20, 0, 0]
        );
        assert_eq!(endpoint::ENDPOINT_PROTOCOL_GENERATION, 1);
        assert_eq!(wire::PROTOCOL_VERSION, 22);
    }
}
