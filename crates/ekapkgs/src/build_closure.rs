//! Compute the store paths `nix build` will need, consulting the cache.
//!
//! The derivation graph from `nix derivation show -r` lists every output
//! that could be involved in a build, down to the bootstrap tools. Nix only
//! needs a fraction of it: for each output the cache already has, its runtime
//! closure; for each output that must be built, the inputs of its derivation,
//! resolved the same way. This module walks the graph in that order, asking
//! the cache in one round trip per level of uncached derivations.

use std::collections::{BTreeSet, HashMap, HashSet};

use ekapkgs_nix::eval::DerivationShowOutput;
use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval, store};
use ekapkgs_protocol::ekapkgs::v1::PathManifestEntry;

const STORE_DIR: &str = "/nix/store";

/// What the installable is going to be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    /// Build the installable's outputs (`nix build`, `nix run`).
    Outputs,
    /// Only the installable's build inputs are needed (`nix develop`).
    Inputs,
}

/// The cache's answer for a set of wanted paths.
#[derive(Debug, Default)]
pub struct ClosureAnswer {
    /// Paths on the cache, covering the wanted paths' runtime closures.
    pub available: Vec<String>,
    /// Wanted paths, or references of available paths, not on the cache.
    pub unavailable: Vec<String>,
}

/// Compute the paths needed to build `installable` with the cache at
/// `server_url`.
///
/// Falls back to the full derivation graph if the cache can't be queried.
pub fn build_closure_paths(
    server_url: &str,
    installable: &Installable,
    goal: Goal,
) -> color_eyre::Result<Vec<String>> {
    let show: DerivationShowOutput = NixCommand::new(&["derivation", "show"])
        .arg("-r")
        .arg(&installable.raw)
        .json()?;
    let graph = DerivationGraph::from_show(&show);

    let rt = tokio::runtime::Runtime::new()?;
    let planned = plan(
        &graph,
        goal,
        |paths| store::partition_local(paths).map_err(Into::into),
        |paths| rt.block_on(query_closure(server_url, paths)),
    );

    match planned {
        Ok(paths) => Ok(paths),
        Err(e) => {
            tracing::warn!("Could not resolve build closure against cache ({e}), using full graph");
            eval::derivation_closure_paths(installable).map_err(Into::into)
        },
    }
}

/// Ask the cache for the runtime closure of `paths`.
async fn query_closure(server_url: &str, paths: &[String]) -> color_eyre::Result<ClosureAnswer> {
    let response =
        crate::negotiate::negotiate_closure(server_url, store_path_hashes(paths), Vec::new())
            .await?;

    // The response identifies unavailable paths by hash only; recover the
    // full path from the request or from references that name it.
    let mut by_hash: HashMap<String, String> = HashMap::new();
    for path in runtime_closure_paths(paths, &response.available) {
        if let Some(hash) = store::store_path_hash(&path) {
            by_hash.insert(hash.to_owned(), path);
        }
    }

    Ok(ClosureAnswer {
        available: response
            .available
            .iter()
            .map(|e| e.store_path.clone())
            .collect(),
        unavailable: response
            .unavailable
            .iter()
            .filter_map(|h| by_hash.get(h).cloned())
            .collect(),
    })
}

/// A derivation graph keyed by full `.drv` path.
#[derive(Debug, Default)]
pub struct DerivationGraph {
    nodes: HashMap<String, Node>,
    /// Output path → (derivation, output name).
    producers: HashMap<String, (String, String)>,
}

#[derive(Debug, Default)]
struct Node {
    /// Output name → output path. Outputs without a known path (e.g.
    /// floating content-addressed ones) are omitted.
    outputs: HashMap<String, String>,
    /// Input derivation → output names used.
    input_drvs: Vec<(String, Vec<String>)>,
    /// Input source paths.
    srcs: Vec<String>,
}

impl DerivationGraph {
    pub fn from_show(show: &DerivationShowOutput) -> Self {
        let mut graph = Self::default();
        for (drv, info) in &show.derivations {
            let drv = full_path(drv);
            let mut node = Node::default();
            for (name, output) in &info.outputs {
                if let Some(path) = &output.path {
                    let path = full_path(path);
                    graph
                        .producers
                        .insert(path.clone(), (drv.clone(), name.clone()));
                    node.outputs.insert(name.clone(), path);
                }
            }
            if let Some(inputs) = &info.inputs {
                node.input_drvs = inputs
                    .drvs
                    .iter()
                    .map(|(d, i)| (full_path(d), i.outputs().to_vec()))
                    .collect();
                node.srcs = inputs.srcs.iter().map(|s| full_path(s)).collect();
            }
            graph.nodes.insert(drv, node);
        }
        graph
    }

    /// Derivations that no other derivation in the graph depends on.
    fn roots(&self) -> Vec<&str> {
        let inputs: HashSet<&str> = self
            .nodes
            .values()
            .flat_map(|n| n.input_drvs.iter().map(|(d, _)| d.as_str()))
            .collect();
        let mut roots: Vec<&str> = self
            .nodes
            .keys()
            .map(String::as_str)
            .filter(|d| !inputs.contains(d))
            .collect();
        roots.sort_unstable();
        roots
    }
}

/// Walk `graph` from its roots, returning the paths nix needs for `goal`.
///
/// `partition_local` splits paths into (local, missing); `query` asks the
/// cache about missing paths. Outputs that must be built are included, so
/// callers can report them, alongside everything needed to build them.
pub fn plan(
    graph: &DerivationGraph,
    goal: Goal,
    mut partition_local: impl FnMut(&[String]) -> color_eyre::Result<(Vec<String>, Vec<String>)>,
    mut query: impl FnMut(&[String]) -> color_eyre::Result<ClosureAnswer>,
) -> color_eyre::Result<Vec<String>> {
    let mut needed: BTreeSet<String> = BTreeSet::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut expanded: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = Vec::new();

    // Queue the inputs of a derivation that is going to be built.
    let mut expand = |drv: &str, frontier: &mut Vec<String>, needed: &mut BTreeSet<String>| {
        if !expanded.insert(drv.to_owned()) {
            return;
        }
        let Some(node) = graph.nodes.get(drv) else {
            return;
        };
        for (input, outputs) in &node.input_drvs {
            let Some(input_node) = graph.nodes.get(input) else {
                continue;
            };
            for name in outputs {
                if let Some(path) = input_node.outputs.get(name) {
                    frontier.push(path.clone());
                }
            }
        }
        needed.extend(node.srcs.iter().cloned());
    };

    for root in graph.roots() {
        match goal {
            Goal::Outputs => {
                if let Some(node) = graph.nodes.get(root) {
                    frontier.extend(node.outputs.values().cloned());
                }
            },
            Goal::Inputs => expand(root, &mut frontier, &mut needed),
        }
    }

    while !frontier.is_empty() {
        let mut batch: Vec<String> = frontier
            .drain(..)
            .filter(|p| seen.insert(p.clone()))
            .collect();
        if batch.is_empty() {
            break;
        }
        batch.sort();

        let (local, missing) = partition_local(&batch)?;
        // A valid path's closure is always valid, so local paths need no
        // further resolution.
        needed.extend(local);
        if missing.is_empty() {
            continue;
        }

        let answer = query(&missing)?;
        for path in answer.available {
            seen.insert(path.clone());
            needed.insert(path);
        }

        let missing: HashSet<String> = missing.into_iter().collect();
        for path in answer.unavailable {
            if !missing.contains(&path) {
                // A reference of an available path; it may still be local,
                // so check it in the next round before deciding to build it.
                frontier.push(path);
                continue;
            }
            // Neither local nor on the cache, so nix will build it (or
            // substitute it elsewhere, for paths outside the graph).
            if let Some((drv, _)) = graph.producers.get(&path) {
                expand(drv, &mut frontier, &mut needed);
            }
            needed.insert(path);
        }
    }

    Ok(needed.into_iter().collect())
}

/// Extract the hash portion of each store path, skipping malformed ones.
pub fn store_path_hashes(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|p| store::store_path_hash(p).map(String::from))
        .collect()
}

/// Collect the full runtime closure from a closure-expanded negotiate
/// response.
///
/// Includes each available entry and everything it references, plus the
/// requested `roots`. References of paths the server lacks are not known, so
/// those paths are included but not expanded.
pub fn runtime_closure_paths(roots: &[String], available: &[PathManifestEntry]) -> Vec<String> {
    let mut paths: Vec<String> = roots.to_vec();
    for entry in available {
        paths.push(entry.store_path.clone());
        paths.extend(entry.references.iter().map(|r| full_path(r)));
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Normalize a bare `hash-name` to a full store path.
fn full_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{STORE_DIR}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(store_path: &str, references: &[&str]) -> PathManifestEntry {
        PathManifestEntry {
            store_path: store_path.to_owned(),
            references: references.iter().map(|r| (*r).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn p(name: &str) -> String {
        format!("/nix/store/{name}")
    }

    /// Build a graph from `(drv, output path, inputs, srcs)` tuples, where
    /// every derivation has a single `out` output.
    fn graph(nodes: &[(&str, &str, &[&str], &[&str])]) -> DerivationGraph {
        let mut graph = DerivationGraph::default();
        for (drv, out, inputs, srcs) in nodes {
            graph.producers.insert(p(out), (p(drv), "out".to_owned()));
            graph.nodes.insert(
                p(drv),
                Node {
                    outputs: HashMap::from([("out".to_owned(), p(out))]),
                    input_drvs: inputs
                        .iter()
                        .map(|i| (p(i), vec!["out".to_owned()]))
                        .collect(),
                    srcs: srcs.iter().map(|s| p(s)).collect(),
                },
            );
        }
        graph
    }

    /// Plan against a fake local store and cache.
    ///
    /// `cache` maps each cached path to its references; references that are
    /// not themselves keys are reported unavailable.
    fn run_plan(
        graph: &DerivationGraph,
        goal: Goal,
        local: &[&str],
        cache: &[(&str, &[&str])],
    ) -> (Vec<String>, Vec<Vec<String>>) {
        let local: HashSet<String> = local.iter().map(|l| p(l)).collect();
        let cache: HashMap<String, Vec<String>> = cache
            .iter()
            .map(|(k, refs)| (p(k), refs.iter().map(|r| p(r)).collect()))
            .collect();
        let mut queries = Vec::new();

        let planned = plan(
            graph,
            goal,
            |paths| Ok(paths.iter().cloned().partition(|p| local.contains(p))),
            |paths| {
                queries.push(paths.to_vec());
                let mut answer = ClosureAnswer::default();
                let mut stack = paths.to_vec();
                let mut seen = HashSet::new();
                while let Some(path) = stack.pop() {
                    if !seen.insert(path.clone()) {
                        continue;
                    }
                    match cache.get(&path) {
                        Some(refs) => {
                            answer.available.push(path);
                            stack.extend(refs.iter().cloned());
                        },
                        None => answer.unavailable.push(path),
                    }
                }
                Ok(answer)
            },
        )
        .unwrap();

        (planned, queries)
    }

    #[test]
    fn cached_output_needs_only_runtime_closure() {
        // hello is cached; its build inputs (stdenv, tarball) must be skipped.
        let g = graph(&[
            ("hello.drv", "hello", &["stdenv.drv", "tarball.drv"], &[]),
            ("stdenv.drv", "stdenv", &[], &[]),
            ("tarball.drv", "tarball", &[], &[]),
        ]);
        let (planned, queries) = run_plan(&g, Goal::Outputs, &["glibc"], &[("hello", &["glibc"])]);

        assert_eq!(planned, vec![p("glibc"), p("hello")]);
        // glibc is unavailable on the cache but local, so it is re-checked
        // locally rather than queried again or built.
        assert_eq!(queries, vec![vec![p("hello")]]);
    }

    #[test]
    fn uncached_output_expands_to_inputs() {
        // The toplevel must be built; one input is cached, one is local, and
        // one must itself be built from a cached input and a source.
        let g = graph(&[
            (
                "top.drv",
                "top",
                &["etc.drv", "kernel.drv", "local.drv"],
                &[],
            ),
            ("etc.drv", "etc", &["bash.drv"], &["etc-src"]),
            ("kernel.drv", "kernel", &["gcc.drv"], &[]),
            ("bash.drv", "bash", &[], &[]),
            ("local.drv", "localdep", &[], &[]),
            ("gcc.drv", "gcc", &[], &[]),
        ]);
        let (planned, queries) = run_plan(
            &g,
            Goal::Outputs,
            &["localdep"],
            &[("kernel", &[]), ("bash", &[])],
        );

        assert_eq!(
            planned,
            vec![
                p("bash"),
                p("etc"),
                p("etc-src"),
                p("kernel"),
                p("localdep"),
                p("top"),
            ]
        );
        // gcc is only a build input of the cached kernel, so it is skipped.
        assert!(!planned.contains(&p("gcc")));
        assert_eq!(
            queries,
            vec![
                vec![p("top")],
                vec![p("etc"), p("kernel")],
                // Sources are added to the store at evaluation time, so they
                // are recorded without asking the cache.
                vec![p("bash")],
            ]
        );
    }

    #[test]
    fn inputs_goal_skips_root_output() {
        // A dev shell's own output is never built.
        let g = graph(&[
            ("shell.drv", "shell", &["rustc.drv"], &[]),
            ("rustc.drv", "rustc", &["llvm.drv"], &[]),
            ("llvm.drv", "llvm", &[], &[]),
        ]);
        let (planned, queries) = run_plan(&g, Goal::Inputs, &[], &[("rustc", &[])]);

        assert_eq!(planned, vec![p("rustc")]);
        assert_eq!(queries, vec![vec![p("rustc")]]);
    }

    #[test]
    fn local_outputs_skip_the_cache() {
        let g = graph(&[
            ("hello.drv", "hello", &["stdenv.drv"], &[]),
            ("stdenv.drv", "stdenv", &[], &[]),
        ]);
        let (planned, queries) = run_plan(&g, Goal::Outputs, &["hello"], &[]);

        assert_eq!(planned, vec![p("hello")]);
        assert!(queries.is_empty());
    }

    #[test]
    fn runtime_closure_includes_references() {
        let roots = vec!["/nix/store/aaa-hello".to_owned()];
        let available = vec![
            entry("/nix/store/aaa-hello", &["aaa-hello", "bbb-glibc"]),
            entry(
                "/nix/store/bbb-glibc",
                &["/nix/store/bbb-glibc", "ccc-libidn"],
            ),
        ];

        assert_eq!(
            runtime_closure_paths(&roots, &available),
            vec![
                "/nix/store/aaa-hello",
                "/nix/store/bbb-glibc",
                "/nix/store/ccc-libidn",
            ]
        );
    }

    #[test]
    fn runtime_closure_keeps_roots_missing_from_server() {
        let roots = vec!["/nix/store/zzz-unknown".to_owned()];

        assert_eq!(
            runtime_closure_paths(&roots, &[]),
            vec!["/nix/store/zzz-unknown"]
        );
    }

    #[test]
    fn store_path_hashes_extracts_hashes() {
        let paths = vec![
            "/nix/store/aaa-hello-2.12".to_owned(),
            "/nix/store/bbb-glibc".to_owned(),
        ];

        assert_eq!(store_path_hashes(&paths), vec!["aaa", "bbb"]);
    }
}
