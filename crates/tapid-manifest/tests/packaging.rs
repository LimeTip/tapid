use tapid_manifest::PackageManifest;

#[test]
fn packaging_preserves_absent_and_empty_selection_and_exact_metadata() {
    let manifest = PackageManifest::parse(r#"{"name":"tool","version":"1.0.0","private":true,"files":["dist/**"],"main":"./dist/index.js","bin":"cli.js","custom":{"kept":true}}"#).unwrap();
    let metadata = manifest.packaging().unwrap();
    assert_eq!(metadata.files.unwrap(), ["dist/**"]);
    assert_eq!(metadata.main, Some("./dist/index.js"));
    assert_eq!(
        manifest.bin().unwrap().targets()[0].target.to_str(),
        Some("cli.js")
    );
    let serialized = manifest.to_json();
    assert!(serialized.contains("\"custom\""));
    let absent = PackageManifest::new("tool", "1.0.0", false).unwrap();
    assert!(absent.packaging().unwrap().files.is_none());
    let empty = PackageManifest::parse(r#"{"name":"tool","version":"1.0.0","files":[]}"#).unwrap();
    assert_eq!(empty.packaging().unwrap().files, Some(vec![]));
}

#[test]
fn packaging_rejects_unsupported_declarations_only_when_requested() {
    for declaration in [
        r#""files":null"#,
        r#""files":"dist""#,
        r#""files":[1]"#,
        r#""main":false"#,
        r#""main":"""#,
        r#""main":"../escape""#,
        r#""main":"C:/escape""#,
        r#""main":"a\\b""#,
        r#""bundleDependencies":true"#,
        r#""bundleDependencies":["dep"]"#,
        r#""bundledDependencies":{}"#,
        r#""bundledDependencies":[1]"#,
        r#""directories":{"bin":"bin"}"#,
        r#""directories":false"#,
    ] {
        let input = format!(r#"{{"name":"tool","version":"1.0.0",{declaration}}}"#);
        let manifest = PackageManifest::parse(&input).unwrap();
        assert!(manifest.packaging().is_err(), "accepted {declaration}");
        let roundtrip = PackageManifest::parse(&manifest.to_json()).unwrap();
        assert_eq!(manifest, roundtrip);
    }
    for declaration in [
        r#""bundleDependencies":false"#,
        r#""bundleDependencies":[]"#,
        r#""bundledDependencies":false"#,
        r#""bundledDependencies":[]"#,
        r#""directories":{"lib":"src"}"#,
    ] {
        let input = format!(r#"{{"name":"tool","version":"1.0.0",{declaration}}}"#);
        assert!(PackageManifest::parse(&input).unwrap().packaging().is_ok());
    }
}
