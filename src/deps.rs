//! The dependency graph of a scanned closure: canonical-key resolution and
//! dependency-loop detection.
//!
//! A loop is a real condition in the ports tree — usually option-dependent,
//! sometimes only in one direction of a flavor pair — and poudriere cannot
//! build a port that is its own (transitive) dependency. optique sees the
//! whole closure before any build starts, so it is the place to say so.
//!
//! Only the edges poudriere builds count. `TEST_DEPENDS` are part of the
//! closure but are built only under `bulk -t`, and the ports tree's test
//! dependencies are cyclic on a grand scale (hundreds of ports in one
//! component), so an edge no other dependency list asks for is left out.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;

/// Resolve a (possibly non-canonical) key to the key the closure stores the
/// port under. Dep edges and command-line arguments may name a port without
/// its flavor, while the scan files it under the flavor `make` selected.
pub fn resolve(
    ports: &BTreeMap<PortKey, PortInfo>,
    aliases: &HashMap<PortKey, PortKey>,
    key: &PortKey,
) -> Option<PortKey> {
    if ports.contains_key(key) {
        return Some(key.clone());
    }
    aliases.get(key).cloned()
}

/// One dependency loop found in the closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepLoop {
    /// Every port tangled in it — the whole strongly connected component, in
    /// key order. Two ports of a component need not sit on the same cycle,
    /// but each can reach the other, so none can be built first.
    pub members: Vec<PortKey>,
    /// A shortest closed walk through `members[0]`, listed once: the walk
    /// returns to its first element, which is not repeated here.
    pub cycle: Vec<PortKey>,
}

impl DepLoop {
    /// The walk as one line, start repeated so the loop closes visibly:
    /// `devel/a → devel/b → devel/a`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for key in &self.cycle {
            out.push_str(&key.to_string());
            out.push_str(" → ");
        }
        match self.cycle.first() {
            Some(first) => out.push_str(&first.to_string()),
            None => out.clear(),
        }
        out
    }

    /// Is this port part of the loop?
    pub fn contains(&self, key: &PortKey) -> bool {
        self.members.iter().any(|m| m == key)
    }

    /// Ports in the tangle that the walk does not visit — a component can be
    /// larger than any single cycle through it.
    pub fn extra_members(&self) -> Vec<&PortKey> {
        let walked: std::collections::HashSet<&PortKey> = self.cycle.iter().collect();
        self.members.iter().filter(|m| !walked.contains(m)).collect()
    }

    /// Those extra ports as one line, or None when the walk covers the whole
    /// tangle. Long tangles are cut to `EXTRA_SHOWN` names and the rest
    /// counted: a report no terminal can show is no report.
    pub fn extra_line(&self) -> Option<String> {
        let extra = self.extra_members();
        if extra.is_empty() {
            return None;
        }
        let shown: Vec<String> =
            extra.iter().take(EXTRA_SHOWN).map(|k| k.to_string()).collect();
        let mut line = shown.join(" ");
        if let Some(rest) = extra.len().checked_sub(EXTRA_SHOWN).filter(|r| *r > 0) {
            line.push_str(&format!(" + {rest} more"));
        }
        Some(line)
    }
}

/// How many of a tangle's extra ports a one-line report names before counting
/// the rest.
pub const EXTRA_SHOWN: usize = 12;

/// Every dependency loop in the closure, ordered by their first member.
///
/// Strongly connected components of two or more ports are loops by
/// definition; a lone port counts only when it really depends on itself.
pub fn detect(
    ports: &BTreeMap<PortKey, PortInfo>,
    aliases: &HashMap<PortKey, PortKey>,
) -> Vec<DepLoop> {
    let graph = Graph::build(ports, aliases);
    graph
        .components()
        .into_iter()
        .filter(|comp| comp.len() > 1 || graph.adj[comp[0]].contains(&comp[0]))
        .map(|comp| DepLoop {
            members: comp.iter().map(|&i| graph.nodes[i].clone()).collect(),
            cycle: graph
                .shortest_cycle(&comp)
                .into_iter()
                .map(|i| graph.nodes[i].clone())
                .collect(),
        })
        .collect()
}

/// The closure as an index-addressed adjacency list. Node order is the
/// closure's key order and every neighbor list is sorted and deduplicated,
/// so everything downstream — components, cycles, reports — is deterministic.
struct Graph {
    nodes: Vec<PortKey>,
    adj: Vec<Vec<usize>>,
}

impl Graph {
    fn build(ports: &BTreeMap<PortKey, PortInfo>, aliases: &HashMap<PortKey, PortKey>) -> Self {
        let nodes: Vec<PortKey> = ports.keys().cloned().collect();
        let index: HashMap<&PortKey, usize> =
            nodes.iter().enumerate().map(|(i, k)| (k, i)).collect();
        let adj = nodes
            .iter()
            .map(|key| {
                let mut targets: Vec<usize> = ports[key]
                    .deps
                    .iter()
                    // Test-only edges are in the closure but not in the build
                    // order: poudriere builds them only under `bulk -t`, and
                    // counting them would report the ports tree's whole
                    // test-dependency tangle as a loop.
                    .filter(|dep| !dep.test_only)
                    // A dep outside the closure (a port that failed to query,
                    // say) cannot close a loop inside it.
                    .filter_map(|dep| resolve(ports, aliases, &dep.target))
                    .filter_map(|target| index.get(&target).copied())
                    .collect();
                targets.sort_unstable();
                targets.dedup();
                targets
            })
            .collect();
        Graph { nodes, adj }
    }

    /// Strongly connected components, smallest node index first within each
    /// and components ordered by that index. Tarjan's algorithm with an
    /// explicit call stack: a closure is a thousand ports deep in the worst
    /// case, and recursion would put that on the real one.
    fn components(&self) -> Vec<Vec<usize>> {
        let n = self.nodes.len();
        const UNVISITED: usize = usize::MAX;
        let mut index = vec![UNVISITED; n];
        let mut low = vec![0usize; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        let mut next = 0usize;
        let mut comps: Vec<Vec<usize>> = Vec::new();

        for root in 0..n {
            if index[root] != UNVISITED {
                continue;
            }
            index[root] = next;
            low[root] = next;
            next += 1;
            stack.push(root);
            on_stack[root] = true;
            // (node, how many of its neighbors have been walked)
            let mut call: Vec<(usize, usize)> = vec![(root, 0)];
            while let Some(&(v, cursor)) = call.last() {
                if let Some(&w) = self.adj[v].get(cursor) {
                    call.last_mut().expect("just observed").1 = cursor + 1;
                    if index[w] == UNVISITED {
                        index[w] = next;
                        low[w] = next;
                        next += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        call.push((w, 0));
                    } else if on_stack[w] {
                        low[v] = low[v].min(index[w]);
                    }
                    continue;
                }
                call.pop();
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    comp.sort_unstable();
                    comps.push(comp);
                }
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
        comps.sort_unstable_by_key(|comp| comp[0]);
        comps
    }

    /// A shortest closed walk through `comp[0]`, that node first and listed
    /// once. Breadth-first inside the component, so the first node popped
    /// with an edge back to the start is at the smallest possible distance.
    /// Every member of a strongly connected component lies on some cycle, so
    /// the walk always exists.
    fn shortest_cycle(&self, comp: &[usize]) -> Vec<usize> {
        let start = comp[0];
        let inside: Vec<bool> = {
            let mut m = vec![false; self.nodes.len()];
            for &i in comp {
                m[i] = true;
            }
            m
        };
        let mut pred: HashMap<usize, usize> = HashMap::new();
        let mut seen = vec![false; self.nodes.len()];
        let mut queue: VecDeque<usize> = VecDeque::from([start]);
        seen[start] = true;
        while let Some(v) = queue.pop_front() {
            for &w in &self.adj[v] {
                if !inside[w] {
                    continue;
                }
                if w == start {
                    // Walk the predecessors back to the start, then read the
                    // cycle forwards from it.
                    let mut walk = vec![v];
                    while let Some(&p) = pred.get(walk.last().expect("walk is never empty")) {
                        walk.push(p);
                    }
                    walk.reverse();
                    return walk;
                }
                if !seen[w] {
                    seen[w] = true;
                    pred.insert(w, v);
                    queue.push_back(w);
                }
            }
        }
        // Unreachable for a real component; a bare start beats a panic.
        vec![start]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::options::PortOptions;
    use crate::model::port::DepEdge;

    /// A port with the given dep edges and nothing else of interest. A dep
    /// written `test:cat/x` is an edge only `TEST_DEPENDS` asks for.
    fn linked(origin: &str, deps: &[&str]) -> PortInfo {
        let key = PortKey::parse(origin).expect("test origin parses");
        PortInfo {
            key: key.clone(),
            canonical: key,
            pkgname: format!("{}-1.0", origin.split('/').next_back().unwrap()),
            flavors: vec![],
            options_name: origin.replace('/', "_"),
            options: PortOptions::default(),
            deps: deps
                .iter()
                .map(|d| match d.strip_prefix("test:") {
                    Some(target) => DepEdge {
                        target: PortKey::parse(target).expect("test dep parses"),
                        spec: format!("dep:{target}"),
                        test_only: true,
                    },
                    None => DepEdge {
                        target: PortKey::parse(d).expect("test dep parses"),
                        spec: format!("dep:{d}"),
                        test_only: false,
                    },
                })
                .collect(),
            broken: None,
            ignore: None,
            deprecated: None,
            pkg_help: None,
            default_versions: vec![],
            warnings: vec![],
        }
    }

    fn closure(ports: &[(&str, &[&str])]) -> BTreeMap<PortKey, PortInfo> {
        ports
            .iter()
            .map(|(origin, deps)| {
                let info = linked(origin, deps);
                (info.canonical.clone(), info)
            })
            .collect()
    }

    fn loops(ports: &[(&str, &[&str])]) -> Vec<DepLoop> {
        detect(&closure(ports), &HashMap::new())
    }

    fn rendered(ports: &[(&str, &[&str])]) -> Vec<String> {
        loops(ports).iter().map(DepLoop::render).collect()
    }

    #[test]
    fn an_acyclic_closure_has_no_loops() {
        assert!(loops(&[
            ("cat/root", &["cat/mid", "cat/leaf"]),
            ("cat/mid", &["cat/leaf"]),
            ("cat/leaf", &[]),
        ])
        .is_empty());
    }

    /// A diamond revisits ports without ever closing a walk.
    #[test]
    fn shared_dependencies_are_not_loops() {
        assert!(loops(&[
            ("cat/root", &["cat/left", "cat/right"]),
            ("cat/left", &["cat/leaf"]),
            ("cat/right", &["cat/leaf"]),
            ("cat/leaf", &[]),
        ])
        .is_empty());
    }

    #[test]
    fn a_two_port_loop_is_reported_once() {
        let found = loops(&[
            ("cat/root", &["cat/a"]),
            ("cat/a", &["cat/b"]),
            ("cat/b", &["cat/a"]),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].members,
            vec![PortKey::parse("cat/a").unwrap(), PortKey::parse("cat/b").unwrap()]
        );
        assert_eq!(found[0].render(), "cat/a → cat/b → cat/a");
        assert!(found[0].extra_members().is_empty());
        assert!(found[0].contains(&PortKey::parse("cat/b").unwrap()));
        assert!(!found[0].contains(&PortKey::parse("cat/root").unwrap()));
    }

    /// A port that depends on itself is a loop of one; a port that merely
    /// lists the same dependency twice is not.
    #[test]
    fn self_dependency_is_a_loop_but_a_repeated_edge_is_not() {
        let found = loops(&[("cat/a", &["cat/a", "cat/b"]), ("cat/b", &[])]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].members, vec![PortKey::parse("cat/a").unwrap()]);
        assert_eq!(found[0].render(), "cat/a → cat/a");

        assert!(loops(&[("cat/a", &["cat/b", "cat/b"]), ("cat/b", &[])]).is_empty());
    }

    /// Two independent tangles stay independent, ordered by first member.
    #[test]
    fn separate_loops_are_listed_separately_in_key_order() {
        assert_eq!(
            rendered(&[
                ("cat/y", &["cat/z"]),
                ("cat/z", &["cat/y"]),
                ("cat/a", &["cat/b"]),
                ("cat/b", &["cat/a"]),
            ]),
            vec!["cat/a → cat/b → cat/a", "cat/y → cat/z → cat/y"]
        );
    }

    /// The walk printed is a shortest one, not whichever the search met
    /// first: a → b → a beats a → c → d → a through the same component.
    #[test]
    fn the_reported_walk_is_a_shortest_one() {
        let found = loops(&[
            ("cat/a", &["cat/c", "cat/b"]),
            ("cat/b", &["cat/a"]),
            ("cat/c", &["cat/d"]),
            ("cat/d", &["cat/a"]),
        ]);
        assert_eq!(found.len(), 1, "one component, four ports");
        assert_eq!(found[0].render(), "cat/a → cat/b → cat/a");
        assert_eq!(found[0].members.len(), 4);
        // c and d are tangled too, even though the shortest walk skips them.
        assert_eq!(
            found[0].extra_members(),
            vec![&PortKey::parse("cat/c").unwrap(), &PortKey::parse("cat/d").unwrap()]
        );
    }

    /// Nested loops sharing a port are one component: neither port can be
    /// built before the other, so reporting them apart would mislead.
    #[test]
    fn loops_sharing_a_port_are_one_component() {
        let found = loops(&[
            ("cat/a", &["cat/b"]),
            ("cat/b", &["cat/a", "cat/c"]),
            ("cat/c", &["cat/b"]),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].members.len(), 3);
    }

    /// Dep edges name a port without its flavor; the closure files it under
    /// the flavor make chose. A loop must survive that indirection.
    #[test]
    fn loops_are_found_through_flavor_aliases() {
        let mut ports = closure(&[("cat/a", &["cat/b"])]);
        let flavored = {
            let mut info = linked("cat/b", &["cat/a"]);
            info.canonical = PortKey::parse("cat/b@py312").unwrap();
            info
        };
        ports.insert(flavored.canonical.clone(), flavored);
        let mut aliases = HashMap::new();
        aliases.insert(PortKey::parse("cat/b").unwrap(), PortKey::parse("cat/b@py312").unwrap());

        let found = detect(&ports, &aliases);
        assert_eq!(found.len(), 1, "the alias closes the loop");
        assert_eq!(found[0].render(), "cat/a → cat/b@py312 → cat/a");
        // Without the alias table the edge dangles outside the closure.
        assert!(detect(&ports, &HashMap::new()).is_empty());
    }

    /// A tangle too wide for a line is cut and the rest counted.
    #[test]
    fn the_extra_members_line_is_capped() {
        // A hub every spoke depends on, and which depends on every spoke:
        // one component of 1 + n ports whose shortest walk is hub → spoke.
        let n = EXTRA_SHOWN + 5;
        let spokes: Vec<String> = (0..n).map(|i| format!("cat/s{i:03}")).collect();
        let hub_deps: Vec<&str> = spokes.iter().map(String::as_str).collect();
        let mut ports = closure(&[("cat/hub", &hub_deps)]);
        for spoke in &spokes {
            let info = linked(spoke, &["cat/hub"]);
            ports.insert(info.canonical.clone(), info);
        }
        let found = detect(&ports, &HashMap::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].members.len(), n + 1);
        let line = found[0].extra_line().expect("the walk cannot cover them all");
        assert_eq!(line.split_whitespace().filter(|w| w.starts_with("cat/")).count(), EXTRA_SHOWN);
        assert!(line.ends_with("+ 4 more"), "{line}");

        // A tangle the walk covers entirely has no such line at all.
        let two = loops(&[("cat/a", &["cat/b"]), ("cat/b", &["cat/a"])]);
        assert_eq!(two[0].extra_line(), None);
    }

    /// The ports tree's test dependencies are cyclic on a grand scale, and
    /// poudriere does not build them unless asked: an edge only
    /// `TEST_DEPENDS` wants closes nothing.
    #[test]
    fn test_only_edges_do_not_close_a_loop() {
        assert!(loops(&[("cat/a", &["cat/b"]), ("cat/b", &["test:cat/a"])]).is_empty());
        // The same pair with a real edge back is a loop again.
        assert_eq!(loops(&[("cat/a", &["cat/b"]), ("cat/b", &["cat/a"])]).len(), 1);
        // A port that only tests against itself is not a loop either.
        assert!(loops(&[("cat/a", &["test:cat/a"])]).is_empty());
    }

    /// Dependencies pointing outside the closure (a port whose query failed,
    /// a blacklisted leaf) cannot close a loop.
    #[test]
    fn edges_leaving_the_closure_are_ignored() {
        assert!(loops(&[("cat/a", &["cat/absent"])]).is_empty());
    }

    #[test]
    fn empty_closure_is_handled() {
        assert!(detect(&BTreeMap::new(), &HashMap::new()).is_empty());
    }

    /// A long chain must not recurse: the walk is explicit, so depth costs
    /// heap, not stack.
    #[test]
    fn a_deep_chain_does_not_recurse() {
        let n = 50_000;
        let mut ports = BTreeMap::new();
        for i in 0..n {
            // Zero-padded so the key order is the chain order.
            let origin = format!("cat/p{i:06}");
            let deps: Vec<String> = if i + 1 < n {
                vec![format!("cat/p{:06}", i + 1)]
            } else {
                // The last link closes back onto the first: one component
                // spanning every port.
                vec!["cat/p000000".to_string()]
            };
            let refs: Vec<&str> = deps.iter().map(String::as_str).collect();
            let info = linked(&origin, &refs);
            ports.insert(info.canonical.clone(), info);
        }
        let found = detect(&ports, &HashMap::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].members.len(), n);
        assert_eq!(found[0].cycle.len(), n);
    }
}
