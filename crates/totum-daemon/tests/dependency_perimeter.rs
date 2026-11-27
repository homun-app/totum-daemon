//! Perimetro effettivo delle dipendenze, verificato sui metadati di Cargo.
//!
//! Il lockfile da solo non distingue una crate del workspace da una copia
//! spostata fuori dal checkout con lo stesso nome. Il test interroga quindi
//! `cargo metadata` e verifica l'appartenenza al workspace, la posizione dei
//! pacchetti locali, la provenienza dei pacchetti esterni e i tipi di
//! dipendenza (normale, di sviluppo, di build). Le sostituzioni di sorgente
//! pertinenti (`[patch]`, `[replace]`) compaiono nei metadati come pacchetti
//! locali o da un'altra sorgente e ricadono negli stessi controlli.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

const WORKSPACE_PACKAGES: [&str; 3] = ["totum-cli", "totum-contract", "totum-daemon"];

/// Dipendenze esterne ammesse: `serde` e `serde_json` per i contratti D2 e per
/// la configurazione D3 P3, più `rustix` su Unix e `windows-sys` su Windows per
/// il modulo `platform` di D3, con il loro intero albero dal registro pubblico.
const APPROVED_DEPENDENCIES: [&str; 18] = [
    "bitflags",
    "errno",
    "itoa",
    "libc",
    "linux-raw-sys",
    "memchr",
    "proc-macro2",
    "quote",
    "rustix",
    "serde",
    "serde_core",
    "serde_derive",
    "serde_json",
    "syn",
    "unicode-ident",
    "windows-link",
    "windows-sys",
    "zmij",
];

/// Dipendenze esterne dirette ammesse per ogni pacchetto del workspace, con i
/// tipi effettivamente dichiarati: normali, di sviluppo o di build.
const WORKSPACE_EDGES: [(&str, &str, &[&str]); 6] = [
    ("totum-contract", "serde", &["normal"]),
    ("totum-contract", "serde_json", &["normal"]),
    ("totum-daemon", "rustix", &["normal", "dev"]),
    ("totum-daemon", "serde", &["normal"]),
    ("totum-daemon", "serde_json", &["normal"]),
    // Il nome dell'edge è quello della dichiarazione Cargo, non del pacchetto.
    ("totum-daemon", "windows_sys", &["normal"]),
];

/// Dipendenze esterne dirette ammesse fra pacchetti del registro: ogni altra
/// combinazione allarga il perimetro senza una decisione registrata.
const EXTERNAL_EDGES: [(&str, &[&str]); 10] = [
    ("errno", &["libc", "windows_sys"]),
    (
        "rustix",
        &[
            "bitflags",
            "libc_errno",
            "libc",
            "linux_raw_sys",
            "windows_sys",
        ],
    ),
    ("proc-macro2", &["unicode_ident"]),
    ("quote", &["proc_macro2"]),
    ("serde", &["serde_core", "serde_derive"]),
    ("serde_core", &["serde_derive"]),
    ("serde_derive", &["proc_macro2", "quote", "syn"]),
    (
        "serde_json",
        &["itoa", "memchr", "serde", "serde_core", "zmij"],
    ),
    ("syn", &["proc_macro2", "quote", "unicode_ident"]),
    ("windows-sys", &["windows_link"]),
];

const CRATES_IO_SOURCES: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "registry+https://index.crates.io/",
];

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    workspace_root: PathBuf,
    resolve: Resolve,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    deps: Vec<Edge>,
}

#[derive(Debug, Deserialize)]
struct Edge {
    name: String,
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
struct DepKind {
    kind: Option<String>,
}

fn normalized_kind(kind: Option<&str>) -> &str {
    kind.unwrap_or("normal")
}

fn declared_kinds(edge: &Edge) -> BTreeSet<String> {
    edge.dep_kinds
        .iter()
        .map(|dep| normalized_kind(dep.kind.as_deref()).to_owned())
        .collect()
}

fn edge_target<'a>(packages: &'a BTreeMap<&str, &'a Package>, edge: &Edge) -> &'a Package {
    packages
        .get(edge.pkg.as_str())
        .unwrap_or_else(|| panic!("dependency {} is missing from the package list", edge.pkg))
}

/// I metadati risolvono le dipendenze per tutti i target: `windows-sys` compare
/// anche sui sistemi Unix e viceversa, quindi la politica è per coppia e tipo,
/// non per sistema operativo corrente.
fn assert_workspace_edges<'a>(
    resolves: &BTreeMap<&'a str, &'a Node>,
    packages: &BTreeMap<&'a str, &'a Package>,
    members: &BTreeSet<&str>,
) {
    let mut seen = BTreeSet::new();
    for node in resolves.values() {
        if !members.contains(node.id.as_str()) {
            continue;
        }
        let origin = packages[node.id.as_str()];
        for edge in &node.deps {
            if members.contains(edge_target(packages, edge).id.as_str()) {
                continue;
            }
            let actual = declared_kinds(edge);
            let allowed = WORKSPACE_EDGES
                .iter()
                .find(|(package, name, _)| *package == origin.name && *name == edge.name)
                .unwrap_or_else(|| {
                    panic!(
                        "workspace package `{}` depends directly on `{}` outside the D3 P1-P8 perimeter",
                        origin.name, edge.name
                    )
                });
            let expected: BTreeSet<String> =
                allowed.2.iter().map(|kind| (*kind).to_owned()).collect();
            assert_eq!(
                actual, expected,
                "dependency `{}` of `{}` has kinds {actual:?}, expected {expected:?}",
                edge.name, origin.name
            );
            seen.insert((origin.name.clone(), edge.name.clone()));
        }
    }
    for (package, dependency, _) in WORKSPACE_EDGES {
        assert!(
            seen.contains(&(package.to_owned(), dependency.to_owned())),
            "expected direct dependency `{dependency}` of `{package}` is missing"
        );
    }
}

fn assert_external_edges(
    resolves: &BTreeMap<&str, &Node>,
    packages: &BTreeMap<&str, &Package>,
    members: &BTreeSet<&str>,
) {
    for (id, node) in resolves {
        if members.contains(id) {
            continue;
        }
        let origin = packages[id];
        for edge in &node.deps {
            let Some((_, allowed)) = EXTERNAL_EDGES.iter().find(|(name, _)| *name == origin.name)
            else {
                assert!(
                    edge_target(packages, edge).source.is_none(),
                    "package `{}` depends on `{}` outside the approved registry graph",
                    origin.name,
                    edge.name
                );
                continue;
            };
            assert!(
                allowed.contains(&edge.name.as_str()),
                "package `{}` depends on `{}` outside the approved registry graph",
                origin.name,
                edge.name
            );
        }
    }
}

fn read_metadata() -> Metadata {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Cargo.toml")
        .canonicalize()
        .expect("the workspace manifest must exist");
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("cargo metadata must run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata emits JSON")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the checkout root must exist")
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|error| panic!("cannot canonicalize {}: {error}", path.display()))
}

fn is_approved(package: &Package) -> bool {
    package
        .source
        .as_deref()
        .is_some_and(|source| CRATES_IO_SOURCES.contains(&source))
        && APPROVED_DEPENDENCIES.contains(&package.name.as_str())
}

#[test]
fn resolved_graph_admits_only_workspace_members_and_approved_registry_crates() {
    let metadata = read_metadata();
    let root = workspace_root();
    assert_eq!(
        canonical(&metadata.workspace_root),
        root,
        "cargo metadata resolved a different workspace root"
    );

    let packages: BTreeMap<&str, &Package> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();
    let members: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();

    for package in &metadata.packages {
        match &package.source {
            None => {
                assert!(
                    members.contains(package.id.as_str()),
                    "local package `{}` is not a workspace member: a path outside the workspace is outside the perimeter",
                    package.name
                );
                let manifest = canonical(&package.manifest_path);
                assert!(
                    manifest.starts_with(&root),
                    "local package `{}` lives outside the checkout: {}",
                    package.name,
                    manifest.display()
                );
            }
            Some(source) => {
                assert!(
                    CRATES_IO_SOURCES.contains(&source.as_str()),
                    "package `{}` does not come from the public crates.io registry: {source}",
                    package.name
                );
                assert!(
                    APPROVED_DEPENDENCIES.contains(&package.name.as_str()),
                    "dependency `{}` is not approved for D1/D2/D3",
                    package.name
                );
            }
        }
    }
    for expected in APPROVED_DEPENDENCIES {
        assert!(
            metadata
                .packages
                .iter()
                .any(|package| package.name == expected),
            "approved dependency `{expected}` is missing from the resolved graph"
        );
    }

    assert_eq!(
        metadata.workspace_members.len(),
        WORKSPACE_PACKAGES.len(),
        "the workspace admits only the D1/D2/D3 packages"
    );
    let mut member_names = BTreeSet::new();
    for id in &metadata.workspace_members {
        let package = packages
            .get(id.as_str())
            .unwrap_or_else(|| panic!("workspace member {id} is missing from cargo metadata"));
        assert!(
            WORKSPACE_PACKAGES.contains(&package.name.as_str()),
            "unexpected workspace member `{}`",
            package.name
        );
        assert!(
            package.source.is_none(),
            "workspace member `{}` resolves to an external source",
            package.name
        );
        let manifest = canonical(&package.manifest_path);
        assert!(
            manifest.starts_with(&root),
            "workspace member `{}` lives outside the checkout: {}",
            package.name,
            manifest.display()
        );
        assert!(
            member_names.insert(package.name.as_str()),
            "duplicate workspace member `{}`",
            package.name
        );
    }
    for expected in WORKSPACE_PACKAGES {
        assert!(
            member_names.remove(expected),
            "workspace member `{expected}` is missing"
        );
    }

    let resolves: BTreeMap<&str, &Node> = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    for node in resolves.values() {
        if !members.contains(node.id.as_str()) {
            continue;
        }
        let origin = packages[node.id.as_str()];
        for edge in &node.deps {
            for dep_kind in &edge.dep_kinds {
                if let Some(kind) = &dep_kind.kind {
                    assert!(
                        matches!(kind.as_str(), "dev" | "build"),
                        "unknown dependency kind `{kind}` for `{}`",
                        edge.name
                    );
                }
            }
            let target = edge_target(&packages, edge);
            assert!(
                members.contains(target.id.as_str()) || is_approved(target),
                "workspace package `{}` depends on `{}` outside the D3 P1-P8 perimeter",
                origin.name,
                edge.name
            );
        }
    }

    assert_workspace_edges(&resolves, &packages, &members);
    assert_external_edges(&resolves, &packages, &members);
}

#[test]
fn lockfile_never_references_git_paths_or_other_registries() {
    let lockfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(&lockfile).expect("Cargo.lock must exist");

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("source = ") {
            let source = value.trim().trim_matches('"');
            assert!(
                CRATES_IO_SOURCES.contains(&source),
                "private or non-registry source in Cargo.lock: {source}"
            );
            assert!(
                !source.starts_with("git+") && !source.starts_with("path+"),
                "git or path sources are outside the perimeter: {source}"
            );
            assert!(
                !source.contains("homun") && !source.contains("localhost"),
                "private source in Cargo.lock: {source}"
            );
        }
    }
}
