use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tapid_publish::{NpmPackOptions, pack_npm};
use tapid_test_support::{TempHome, TempProject};

#[test]
#[ignore = "required offline interoperability lane; provision tests/npm-pack-interop first"]
fn pinned_npm_inspects_and_installs_tapid_bytes_offline() {
    let node = std::env::var_os("TAPID_TEST_NODE")
        .expect("set TAPID_TEST_NODE to an absolute Node executable");
    assert!(Path::new(&node).is_absolute());
    let npm = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/npm-pack-interop/node_modules/npm/bin/npm-cli.js");
    assert!(
        npm.is_file(),
        "provision tests/npm-pack-interop using npm ci --ignore-scripts"
    );
    let source = TempProject::new("npm-interop-source").unwrap();
    let consumer = TempProject::new("npm-interop-consumer").unwrap();
    let home = TempHome::new("npm-interop").unwrap();
    source.write("package.json", br#"{"name":"tapid-offline-example","version":"1.2.3","main":"index.cjs","bin":{"example":"cli.cjs"},"scripts":{"prepack":"touch marker","prepare":"touch marker","postpack":"touch marker","install":"touch marker"}}"#).unwrap();
    source
        .write("index.cjs", b"module.exports = { answer: 42 };\n")
        .unwrap();
    source
        .write("cli.cjs", b"#!/usr/bin/env node\nconsole.log(42);\n")
        .unwrap();
    let artifact = pack_npm(source.path(), &NpmPackOptions::default()).unwrap();
    let archive = consumer.path().join("input.tgz");
    fs::write(&archive, artifact.bytes()).unwrap();
    consumer
        .write(
            "package.json",
            br#"{"name":"consumer","version":"1.0.0","private":true}"#,
        )
        .unwrap();
    let config = home.path().join("empty.npmrc");
    fs::write(&config, "").unwrap();
    let run = |args: &[&std::ffi::OsStr]| -> Output {
        let output = Command::new(&node)
            .arg(&npm)
            .args(args)
            .args(["--offline", "--ignore-scripts", "--no-audit", "--no-fund"])
            .env_clear()
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("TMPDIR", home.path())
            .env("TEMP", home.path())
            .env("TMP", home.path())
            .env("npm_config_cache", home.path().join("cache"))
            .env("npm_config_userconfig", &config)
            .env("npm_config_globalconfig", home.path().join("global.npmrc"))
            .current_dir(consumer.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let version = run(&["--version".as_ref()]);
    assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), "10.9.2");
    let inspection = run(&[
        "pack".as_ref(),
        archive.as_os_str(),
        "--dry-run".as_ref(),
        "--json".as_ref(),
    ]);
    let inspection: serde_json::Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(inspection[0]["name"], "tapid-offline-example");
    assert_eq!(inspection[0]["version"], "1.2.3");
    let inspected_paths = inspection[0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        inspected_paths,
        artifact
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>()
    );
    run(&[
        "install".as_ref(),
        archive.as_os_str(),
        "--package-lock=false".as_ref(),
    ]);
    let installed = consumer.path().join("node_modules/tapid-offline-example");
    assert_eq!(
        fs::read(installed.join("index.cjs")).unwrap(),
        b"module.exports = { answer: 42 };\n"
    );
    assert_eq!(
        fs::read(installed.join("cli.cjs")).unwrap(),
        b"#!/usr/bin/env node\nconsole.log(42);\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(installed.join("cli.cjs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    let evaluated = Command::new(&node).args(["--input-type=module", "-e", "import {createRequire} from 'node:module'; const require=createRequire(import.meta.url); if(require('tapid-offline-example').answer!==42 || (await import('tapid-offline-example')).default.answer!==42) process.exit(1);"])
        .current_dir(consumer.path()).env_clear().output().unwrap();
    assert!(
        evaluated.status.success(),
        "{}",
        String::from_utf8_lossy(&evaluated.stderr)
    );
    assert_eq!(fs::read(&archive).unwrap(), artifact.bytes());
    for directory in [source.path(), consumer.path(), installed.as_path()] {
        assert!(!directory.join("marker").exists());
    }
}
