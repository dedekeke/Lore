use std::collections::HashMap;

use petgraph::graph::UnGraph;
use sqlx::PgPool;
use uuid::Uuid;

use crate::db;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunityResult {
    pub num_communities: usize,
    pub num_assigned: usize,
    pub modularity: f64,
}

/// Load codebase edges, run Louvain community detection, write community_id back to code_chunks.
pub async fn detect_communities(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<CommunityResult, String> {
    let edges = db::codebase_edges::get_all_edges(pool, project_id)
        .await
        .map_err(|e| format!("Failed to load edges: {e}"))?;

    if edges.is_empty() {
        return Ok(CommunityResult {
            num_communities: 0,
            num_assigned: 0,
            modularity: 0.0,
        });
    }

    // Build undirected graph: nodes = unique entity names, edges = connections
    let mut node_map: HashMap<String, u32> = HashMap::new();
    let mut node_names: Vec<String> = Vec::new();
    let mut graph = UnGraph::<(), ()>::new_undirected();

    let get_or_insert = |name: &str,
                         node_map: &mut HashMap<String, u32>,
                         node_names: &mut Vec<String>,
                         graph: &mut UnGraph<(), ()>|
     -> u32 {
        if let Some(&idx) = node_map.get(name) {
            return idx;
        }
        let idx = graph.add_node(()).index() as u32;
        node_map.insert(name.to_string(), idx);
        node_names.push(name.to_string());
        idx
    };

    for edge in &edges {
        let src = get_or_insert(
            &edge.source_entity,
            &mut node_map,
            &mut node_names,
            &mut graph,
        );
        let tgt = get_or_insert(
            &edge.target_entity,
            &mut node_map,
            &mut node_names,
            &mut graph,
        );
        if src != tgt {
            graph.add_edge(src.into(), tgt.into(), ());
        }
    }

    let n = graph.node_count();
    if n == 0 {
        return Ok(CommunityResult {
            num_communities: 0,
            num_assigned: 0,
            modularity: 0.0,
        });
    }

    let communities = louvain(&graph);
    let modularity = compute_modularity(&graph, &communities);

    // Map node index -> community_id, then node_name -> community_id
    let mut assignments: Vec<(String, i32)> = Vec::with_capacity(n);
    for (idx, &comm) in communities.iter().enumerate() {
        assignments.push((node_names[idx].clone(), comm as i32));
    }

    let num_communities = {
        let mut seen = std::collections::HashSet::new();
        for &(_, c) in &assignments {
            seen.insert(c);
        }
        seen.len()
    };

    let num_assigned = db::communities::update_chunk_communities(pool, project_id, &assignments)
        .await
        .map_err(|e| format!("Failed to update communities: {e}"))?;

    Ok(CommunityResult {
        num_communities,
        num_assigned: num_assigned as usize,
        modularity,
    })
}

/// Single-pass Louvain: each node starts in its own community, greedily move to
/// the neighbor community yielding max modularity gain. Repeat until no moves.
pub fn louvain(graph: &UnGraph<(), ()>) -> Vec<usize> {
    let n = graph.node_count();
    let m = graph.edge_count() as f64;
    if m == 0.0 {
        return (0..n).collect();
    }

    let two_m = 2.0 * m;
    let mut community: Vec<usize> = (0..n).collect();

    // degree[i] = number of edges incident to node i
    let degree: Vec<f64> = (0..n)
        .map(|i| graph.neighbors(petgraph::graph::NodeIndex::new(i)).count() as f64)
        .collect();

    // Precompute adjacency list for fast neighbor lookup
    let adj: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            graph
                .neighbors(petgraph::graph::NodeIndex::new(i))
                .map(|nb| nb.index())
                .collect()
        })
        .collect();

    let mut improved = true;
    let mut iterations = 0;
    let max_iterations = 50;

    while improved && iterations < max_iterations {
        improved = false;
        iterations += 1;

        for node in 0..n {
            let current_comm = community[node];
            let ki = degree[node];

            // Count edges to each neighboring community
            let mut comm_edges: HashMap<usize, f64> = HashMap::new();
            for &nb in &adj[node] {
                *comm_edges.entry(community[nb]).or_default() += 1.0;
            }

            // Modularity gain for moving node from current_comm to target_comm:
            // delta_Q = [e_target - ki * sigma_target / 2m] - [e_current + ki * sigma_current / 2m]
            // (simplified Louvain formula)
            let e_current = comm_edges.get(&current_comm).copied().unwrap_or(0.0);

            // Sum of degrees in each community (excluding node itself for current)
            let mut sigma: HashMap<usize, f64> = HashMap::new();
            for (i, &c) in community.iter().enumerate() {
                *sigma.entry(c).or_default() += degree[i];
            }

            let sigma_current = sigma.get(&current_comm).copied().unwrap_or(0.0) - ki;

            let mut best_comm = current_comm;
            let mut best_gain = 0.0;

            for (&target_comm, &e_target) in &comm_edges {
                if target_comm == current_comm {
                    continue;
                }
                let sigma_target = sigma.get(&target_comm).copied().unwrap_or(0.0);
                let gain = (e_target - e_current) / two_m
                    - ki * (sigma_target - sigma_current) / (two_m * two_m);
                if gain > best_gain {
                    best_gain = gain;
                    best_comm = target_comm;
                }
            }

            if best_comm != current_comm {
                community[node] = best_comm;
                improved = true;
            }
        }
    }

    // Renumber communities to 0..k-1
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0;
    for c in &mut community {
        let entry = remap.entry(*c).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        *c = *entry;
    }

    community
}

/// Newman-Girvan modularity: Q = (1/2m) * sum_ij [ A_ij - ki*kj/2m ] * delta(ci, cj)
pub fn compute_modularity(graph: &UnGraph<(), ()>, communities: &[usize]) -> f64 {
    let m = graph.edge_count() as f64;
    if m == 0.0 {
        return 0.0;
    }
    let two_m = 2.0 * m;
    let n = graph.node_count();

    let degree: Vec<f64> = (0..n)
        .map(|i| graph.neighbors(petgraph::graph::NodeIndex::new(i)).count() as f64)
        .collect();

    let mut q = 0.0;
    for edge in graph.edge_indices() {
        if let Some((a, b)) = graph.edge_endpoints(edge) {
            let (i, j) = (a.index(), b.index());
            if communities[i] == communities[j] {
                // Each undirected edge contributes for both (i,j) and (j,i)
                q += 2.0 * (1.0 - degree[i] * degree[j] / two_m);
            }
        }
    }
    q / two_m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two clusters of 3 nodes each, connected by a single bridge edge.
    /// Louvain should detect exactly 2 communities.
    #[test]
    fn test_louvain_two_clusters() {
        let mut g = UnGraph::<(), ()>::new_undirected();
        let a = g.add_node(());
        let b = g.add_node(());
        let c = g.add_node(());
        let d = g.add_node(());
        let e = g.add_node(());
        let f = g.add_node(());

        // Cluster 1: a-b, b-c, a-c (triangle)
        g.add_edge(a, b, ());
        g.add_edge(b, c, ());
        g.add_edge(a, c, ());

        // Cluster 2: d-e, e-f, d-f (triangle)
        g.add_edge(d, e, ());
        g.add_edge(e, f, ());
        g.add_edge(d, f, ());

        // Single bridge
        g.add_edge(c, d, ());

        let communities = louvain(&g);
        assert_eq!(communities.len(), 6);

        // Nodes in same cluster should share community
        assert_eq!(communities[a.index()], communities[b.index()]);
        assert_eq!(communities[b.index()], communities[c.index()]);
        assert_eq!(communities[d.index()], communities[e.index()]);
        assert_eq!(communities[e.index()], communities[f.index()]);

        // The two clusters should be different communities
        assert_ne!(communities[a.index()], communities[d.index()]);

        let num_communities: std::collections::HashSet<_> = communities.iter().collect();
        assert_eq!(num_communities.len(), 2);

        let modularity = compute_modularity(&g, &communities);
        assert!(
            modularity > 0.0,
            "Modularity should be positive for clear clusters"
        );
    }

    /// Verify community assignments map correctly by index -> name
    #[test]
    fn test_community_assignment_mapping() {
        let mut g = UnGraph::<(), ()>::new_undirected();
        let _a = g.add_node(());
        let _b = g.add_node(());
        g.add_edge(_a, _b, ());

        let names = ["foo::bar".to_string(), "baz::qux".to_string()];
        let communities = louvain(&g);

        let assignments: Vec<(String, i32)> = communities
            .iter()
            .enumerate()
            .map(|(idx, &comm)| (names[idx].clone(), comm as i32))
            .collect();

        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].0, "foo::bar");
        assert_eq!(assignments[1].0, "baz::qux");
        // With only one edge, both nodes end up in same community
        assert_eq!(assignments[0].1, assignments[1].1);
    }

    #[test]
    fn test_louvain_empty_graph() {
        let g = UnGraph::<(), ()>::new_undirected();
        let communities = louvain(&g);
        assert!(communities.is_empty());
    }

    #[test]
    fn test_louvain_single_node() {
        let mut g = UnGraph::<(), ()>::new_undirected();
        g.add_node(());
        let communities = louvain(&g);
        assert_eq!(communities, vec![0]);
    }

    #[test]
    fn test_modularity_two_nodes_same_community() {
        let mut g = UnGraph::<(), ()>::new_undirected();
        let a = g.add_node(());
        let b = g.add_node(());
        g.add_edge(a, b, ());
        let communities = vec![0, 0];
        let q = compute_modularity(&g, &communities);
        // Q = (1/2m) * 2*(1 - ki*kj/2m) = 0.5 * 2*(1-0.5) = 0.5
        assert!((q - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_modularity_two_nodes_different_communities() {
        let mut g = UnGraph::<(), ()>::new_undirected();
        let a = g.add_node(());
        let b = g.add_node(());
        g.add_edge(a, b, ());
        let communities = vec![0, 1];
        let q = compute_modularity(&g, &communities);
        // Different communities: no contribution -> Q = 0
        assert!((q - 0.0).abs() < 1e-6);
    }
}
