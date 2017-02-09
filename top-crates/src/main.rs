extern crate hyper;
extern crate hyper_native_tls;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate semver;
extern crate mktemp;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::{self, File};
use std::io::prelude::*;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::Command;

use hyper::client::Client;
use hyper::net::HttpsConnector;
use hyper_native_tls::NativeTlsClient;
use mktemp::Temp;
use semver::Version;

#[derive(Debug, Deserialize)]
struct TopCrates {
    crates: Vec<Crate>,
}

#[derive(Debug, Deserialize)]
struct Crate {
    #[serde(rename="id")]
    name: String,
    #[serde(rename="max_version")]
    version: String,
}

impl Crate {
    fn new<N, V>(name: N, version: V) -> Self
        where N: Into<String>,
              V: Into<String>,
    {
        Crate {
            name: name.into(),
            version: version.into(),
        }
    }
}

fn get_top_crates() -> Vec<Crate> {
    let ssl = NativeTlsClient::new().expect("Unable to build TLS client");
    let connector = HttpsConnector::new(ssl);
    let client = Client::with_connector(connector);

    let res = client
        .get("https://crates.io/api/v1/crates?page=1&per_page=100&sort=downloads")
        .send()
        .expect("Could not fetch top crates");
    assert_eq!(res.status, hyper::Ok);

    let top: TopCrates = serde_json::from_reader(res).expect("Invalid JSON");
    top.crates
}

fn crates_to_toml(crates: Vec<Crate>) -> toml::Value {
    use toml::value::{Value, Table};

    let mut package = Table::new();
    package.insert("authors".into(), Value::Array(vec![Value::String("The Rust Playground".into())]));
    package.insert("name".into(), Value::String("playground".into()));
    package.insert("version".into(), Value::String("0.0.1".into()));

    let mut dependencies = Table::new();
    for Crate { name, version } in crates {
        dependencies.insert(name, Value::String(version));
    }

    let mut result = Table::new();
    result.insert("package".into(), Value::Table(package));
    result.insert("dependencies".into(), Value::Table(dependencies));

    Value::Table(result)
}

fn write_cargo_toml(dir: &Path, cargo_toml: toml::Value) -> PathBuf {
    let toml_file = dir.join("Cargo.toml");

    let f = File::create(&toml_file).expect("Unable to create Cargo.toml");
    let mut f = BufWriter::new(f);
    let data = toml::to_vec(&cargo_toml).expect("Unable to encode TOML");
    f.write_all(&data).expect("Couldn't write Cargo.toml");

    toml_file
}

fn resolve_dependencies(dir: &Path, cargo_toml: toml::Value) -> PathBuf {
    let x = Command::new("cargo")
        .args(&["new", "--bin", "dependencies"])
        .current_dir(dir)
        .status().expect("Couldn't create scratch project");
    assert!(x.success(), "Didn't run cargo new");

    let project_dir = dir.join("dependencies");
    write_cargo_toml(&project_dir, cargo_toml);

    let x = Command::new("cargo")
        .args(&["fetch"])
        .current_dir(&project_dir)
        .status().expect("Couldn't resolve dependencies");
    assert!(x.success(), "Didn't run cargo fetch");

    project_dir.join("Cargo.lock")
}

fn get_lockfile(lockfile_path: &Path) -> toml::Value {
    let f = File::open(lockfile_path).expect("Couldn't open the lockfile");
    let mut f = BufReader::new(f);

    let mut s = String::new();
    f.read_to_string(&mut s).expect("Couldn't read the lockfile");

    toml::from_str(&s).expect("Unable to parse lockfile")
}

fn lockfile_to_crates(lockfile: toml::Value) -> Vec<Crate> {
    let packages = lockfile.get("package").expect("Couldn't find packages");
    let packages = packages.as_array().expect("packages not an array");

    packages.iter().map(|package| {
        let package = package.as_table().expect("not an object");
        let name = package.get("name").expect("missing name").as_str().expect("name not string");
        let version = package.get("version").expect("missing version").as_str().expect("version not string");

        Crate::new(name, version)
    }).collect()
}

fn unique_latest_crates(crates: Vec<Crate>) -> Vec<Crate> {
    let mut uniqs = HashMap::new();
    for Crate { name, version } in crates {
        let version = Version::parse(&version).expect("Invalid version");

        match uniqs.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(version);
            }
            Entry::Occupied(mut entry) => {
                if &version > entry.get() {
                    entry.insert(version);
                }
            }
        }
    }

    uniqs.into_iter().map(|(n, v)| Crate::new(n, v.to_string())).collect()
}

static BLACKLIST: &'static [&'static str] = &[
    "libressl-pnacl-sys", // Fails to build
    "pnacl-build-helper", // Fails to build
    "aster", // Not supported on stable
    "quasi", // Not supported on stable
    "quasi_codegen", // Not supported on stable
    "quasi_macros", // Not supported on stable
    "serde_macros", // Apparently deleted
    "openssl", // Ecosystem is fragmented, only pull in via dependencies
    "openssl-sys", // Ecosystem is fragmented, only pull in via dependencies
    "openssl-sys-extras", // Ecosystem is fragmented, only pull in via dependencies
    "openssl-verify", // Ecosystem is fragmented, only pull in via dependencies
    "redox_syscall", // Not supported on stable
];

fn remove_blacklisted_crates(crates: Vec<Crate>) -> Vec<Crate> {
    crates.into_iter()
        .filter(|&Crate { ref name, .. }| !BLACKLIST.contains(&name.as_str()))
        .collect()
}

fn main() {
    let scratch = Temp::new_dir().expect("unable to make scratch dir");

    let crates = get_top_crates();
    let crates = remove_blacklisted_crates(crates);

    let cargo_toml_rev1 = crates_to_toml(crates);
    let lockfile_path = resolve_dependencies(scratch.as_ref(), cargo_toml_rev1);

    let lockfile = get_lockfile(&lockfile_path);
    let crates_rev2 = lockfile_to_crates(lockfile);

    let crates_rev2 = unique_latest_crates(crates_rev2);
    let crates_rev2 = remove_blacklisted_crates(crates_rev2);
    let cargo_toml_rev2 = crates_to_toml(crates_rev2);

    let result_file = write_cargo_toml(scratch.as_ref(), cargo_toml_rev2);
    fs::rename(result_file, "result.Cargo.toml").expect("Couldn't rename");
}
