use tapid_registry_client::{
    HttpResponse, HttpTransport, NodeEngineRequirement, NpmPackageEvidence, NpmRegistry,
    TransportError,
};

struct Metadata(Vec<u8>);
impl HttpTransport for Metadata {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        assert_eq!(url, "https://registry.npmjs.org/pkg");
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            body: self.0.clone(),
        })
    }
}

#[test]
fn install_and_inspection_preserve_absent_and_malformed_engine_evidence() {
    for (engines, expected) in [
        (None, NodeEngineRequirement::NotDeclared),
        (
            Some(serde_json::json!({"npm":">=10"})),
            NodeEngineRequirement::NotDeclared,
        ),
        (
            Some(serde_json::json!({"node":">=22"})),
            NodeEngineRequirement::Range(">=22".into()),
        ),
        (
            Some(serde_json::json!({"node":""})),
            NodeEngineRequirement::Range("".into()),
        ),
        (
            Some(serde_json::json!(null)),
            NodeEngineRequirement::MalformedEngines,
        ),
        (
            Some(serde_json::json!([])),
            NodeEngineRequirement::MalformedEngines,
        ),
        (
            Some(serde_json::json!({"node":false})),
            NodeEngineRequirement::MalformedNode,
        ),
        (
            Some(serde_json::json!({"node":"latest"})),
            NodeEngineRequirement::Range("latest".into()),
        ),
    ] {
        let mut entry = serde_json::json!({"name":"pkg","version":"1.0.0","dist":{"tarball":"https://registry.npmjs.org/pkg/-/pkg-1.0.0.tgz"}});
        if let Some(engines) = engines {
            entry["engines"] = engines;
        }
        let body =
            serde_json::to_vec(&serde_json::json!({"name":"pkg","versions":{"1.0.0":entry}}))
                .unwrap();
        let origin = "https://registry.npmjs.org".parse().unwrap();
        let evidence = NpmPackageEvidence::from_metadata(
            &origin,
            "pkg".parse().unwrap(),
            &"1.0.0".parse().unwrap(),
            &body,
        )
        .unwrap();
        assert_eq!(evidence.node_engine, expected);
        let selected = NpmRegistry::new(Metadata(body), origin)
            .fetch_with_options("pkg", true)
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].node_engine, expected);
    }
}
