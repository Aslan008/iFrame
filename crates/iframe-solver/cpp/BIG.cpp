#include "BIG.h"
#include "Solver.h"
#include <stack>
#include <iostream>

BinaryImplicationGraph::BinaryImplicationGraph(int num_vars)
    : num_vars(num_vars), num_nodes(2 * num_vars + 2), adj(num_nodes) {
}

int BinaryImplicationGraph::extract_from_clauses(const std::vector<CDCLClause*>& clauses) {
    for (auto& list : adj) {
        list.clear();
    }

    int count = 0;
    for (CDCLClause* c : clauses) {
        if (c->deleted) continue;
        if (c->lits.size() == 2) {
            int l1 = c->lits[0];
            int l2 = c->lits[1];

            // (l1 v l2) <=> (~l1 => l2) and (~l2 => l1)
            int not_l1 = lit_to_idx(-l1);
            int is_l2  = lit_to_idx(l2);
            adj[not_l1].push_back(is_l2);

            int not_l2 = lit_to_idx(-l2);
            int is_l1  = lit_to_idx(l1);
            adj[not_l2].push_back(is_l1);

            count++;
        }
    }
    return count;
}

static inline int find_root_alias(std::vector<int>& var_alias, int lit) {
    if (lit == 0) return 0;
    int var = std::abs(lit);
    int sign = (lit > 0) ? 1 : -1;
    int curr = var;
    int acc_sign = 1;
    while (var_alias[curr] != 0) {
        int next_lit = var_alias[curr];
        acc_sign *= (next_lit > 0) ? 1 : -1;
        curr = std::abs(next_lit);
    }
    if (curr != var) {
        var_alias[var] = acc_sign * curr;
    }
    return sign * acc_sign * curr;
}

bool BinaryImplicationGraph::find_sccs_and_substitute(std::vector<int>& var_alias,
                                                     std::vector<int>& /*level0_units*/,
                                                     int& equiv_count,
                                                     int (*/*find_root_fn*/)(const std::vector<int>&, int)) {
    std::vector<int> dfn(num_nodes, 0);
    std::vector<int> low(num_nodes, 0);
    std::vector<int> scc_id(num_nodes, 0);
    std::vector<bool> in_scc_stack(num_nodes, false);
    std::vector<int> scc_stack;
    scc_stack.reserve(num_nodes);

    int timer = 0;
    int current_scc = 0;

    std::vector<std::vector<int>> components;

    struct DfsFrame {
        int u;
        size_t edge_idx;
    };
    std::vector<DfsFrame> dfs_stack;
    dfs_stack.reserve(num_nodes);

    // Итеративный алгоритм Тарьяна (0 рекурсий, 0% риска переполнения стека Windows)
    for (int start = 2; start < num_nodes; ++start) {
        if (dfn[start] != 0) continue;

        dfn[start] = low[start] = ++timer;
        scc_stack.push_back(start);
        in_scc_stack[start] = true;
        dfs_stack.push_back({start, 0});

        while (!dfs_stack.empty()) {
            DfsFrame& frame = dfs_stack.back();
            int u = frame.u;

            if (frame.edge_idx < adj[u].size()) {
                int v = adj[u][frame.edge_idx++];
                if (dfn[v] == 0) {
                    dfn[v] = low[v] = ++timer;
                    scc_stack.push_back(v);
                    in_scc_stack[v] = true;
                    dfs_stack.push_back({v, 0});
                } else if (in_scc_stack[v]) {
                    low[u] = std::min(low[u], dfn[v]);
                }
            } else {
                // Backtrack
                dfs_stack.pop_back();
                if (!dfs_stack.empty()) {
                    int p = dfs_stack.back().u;
                    low[p] = std::min(low[p], low[u]);
                }

                if (low[u] == dfn[u]) {
                    current_scc++;
                    std::vector<int> scc_nodes;
                    while (true) {
                        int node = scc_stack.back();
                        scc_stack.pop_back();
                        in_scc_stack[node] = false;
                        scc_id[node] = current_scc;
                        scc_nodes.push_back(node);
                        if (node == u) break;
                    }
                    if (scc_nodes.size() > 1) {
                        components.push_back(std::move(scc_nodes));
                    }
                }
            }
        }
    }

    // 1. Проверка на прямое противоречие (x и ~x в одной SCC => UNSAT)
    for (int v = 1; v <= num_vars; ++v) {
        int pos_idx = lit_to_idx(v);
        int neg_idx = lit_to_idx(-v);
        if (scc_id[pos_idx] != 0 && scc_id[pos_idx] == scc_id[neg_idx]) {
            return false; // Доказанный UNSAT
        }
    }

    // 2. Вливание эквивалентностей в Union-Find var_alias
    for (const auto& scc : components) {
        int rep_node = scc[0];
        int rep_lit = idx_to_lit(rep_node);

        for (size_t i = 1; i < scc.size(); ++i) {
            int cur_lit = idx_to_lit(scc[i]);

            int r1 = find_root_alias(var_alias, rep_lit);
            int r2 = find_root_alias(var_alias, cur_lit);

            if (std::abs(r1) != std::abs(r2)) {
                int var_to_alias = std::abs(r2);
                int target_lit = (r2 > 0) ? r1 : -r1;
                var_alias[var_to_alias] = target_lit;
                equiv_count++;
            } else if (r1 == -r2) {
                return false; // Противоречие в корнях => UNSAT
            }
        }
    }

    return true;
}
