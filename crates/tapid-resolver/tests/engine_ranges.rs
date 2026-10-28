use tapid_resolver::Requirement;

#[test]
fn engine_ranges_exclude_dependency_specifications() {
    for raw in [
        "latest",
        "lts",
        "npm:node@22",
        "file:node",
        "git+https://example.invalid/node",
        "workspace:*",
        ">=>22",
        "22 ||",
    ] {
        assert!(Requirement::parse_range(raw).is_err(), "{raw}");
    }
    for (raw, version, expected) in [
        ("", "22.0.0", true),
        ("", "22.0.0-rc.1", false),
        (">=20 <23 || >=24", "22.3.0", true),
        (">=20 <23 || >=24", "23.0.0", false),
        (">=22.0.0-rc.1 <23", "22.0.0-rc.2", true),
        (">=22 <23", "22.0.0-rc.2", false),
        ("22.0.0+vendor", "22.0.0", true),
    ] {
        assert_eq!(
            Requirement::parse_range(raw)
                .unwrap()
                .matches(&version.parse().unwrap()),
            expected,
            "{raw}: {version}"
        );
    }
    // The dependency parser retains its separate tag and alias contracts.
    assert!(
        "latest"
            .parse::<Requirement>()
            .unwrap()
            .dist_tag()
            .is_some()
    );
    assert!("npm:node@22".parse::<Requirement>().unwrap().is_alias());
}
