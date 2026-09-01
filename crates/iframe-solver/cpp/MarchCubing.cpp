#include "MarchCubing.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <queue>
#include <unordered_set>
#include <iostream>

MarchCubingEngine::MarchCubingEngine(int num_vars, const std::vector<std::vector<int>>& base_clauses)
    : num_vars(num_vars), clauses(base_clauses) {}

MarchCubingEngine::~MarchCubingEngine() {}

// Быстрый пробный Unit Propagation для оценки переменной (1-й уровень)
static int probe_up_count(
    int root_lit,
    const std::vector<std::vector<int>>& pos_occ,
    const std::vector<std::vector<int>>& neg_occ,
    const std::vector<std::vector<int>>& clauses,
    std::vector<int8_t>& assigns,
    std::vector<int>& trail
) {
    int start_trail = static_cast<int>(trail.size());
    int root_var = std::abs(root_lit);
    
    if (assigns[root_var] != 0) {
        return (assigns[root_var] == (root_lit > 0 ? 1 : -1)) ? 0 : -1;
    }

    assigns[root_var] = (root_lit > 0 ? 1 : -1);
    trail.push_back(root_lit);

    int qhead = start_trail;
    bool conflict = false;

    while (qhead < static_cast<int>(trail.size()) && !conflict) {
        int p = trail[qhead++];
        const auto& occ = (p > 0) ? neg_occ[p] : pos_occ[-p];

        for (int c_idx : occ) {
            const auto& cl = clauses[c_idx];
            bool sat = false;
            int unassigned_lit = 0;
            int unassigned_count = 0;

            for (int lit : cl) {
                int var = std::abs(lit);
                int val = assigns[var];
                if (val != 0) {
                    if ((val == 1 && lit > 0) || (val == -1 && lit < 0)) {
                        sat = true;
                        break;
                    }
                } else {
                    unassigned_count++;
                    unassigned_lit = lit;
                    if (unassigned_count > 1) break;
                }
            }

            if (sat) continue;

            if (unassigned_count == 0) {
                conflict = true;
                break;
            } else if (unassigned_count == 1) {
                int uvar = std::abs(unassigned_lit);
                assigns[uvar] = (unassigned_lit > 0 ? 1 : -1);
                trail.push_back(unassigned_lit);
            }
        }
    }

    int props = static_cast<int>(trail.size()) - start_trail;

    for (size_t i = start_trail; i < trail.size(); ++i) {
        assigns[std::abs(trail[i])] = 0;
    }
    trail.resize(start_trail);

    return conflict ? -1 : props;
}

// 2-уровневое туннельное зондирование (пробивание стен насквозь в глубь графа)
static int probe_tunnel_up_count(
    int root_lit,
    const std::vector<std::vector<int>>& pos_occ,
    const std::vector<std::vector<int>>& neg_occ,
    const std::vector<std::vector<int>>& clauses,
    std::vector<int8_t>& assigns,
    std::vector<int>& trail,
    const std::vector<int>& var_freq
) {
    int start_trail = static_cast<int>(trail.size());
    int root_var = std::abs(root_lit);

    if (assigns[root_var] != 0) {
        return (assigns[root_var] == (root_lit > 0 ? 1 : -1)) ? 0 : -1;
    }

    assigns[root_var] = (root_lit > 0 ? 1 : -1);
    trail.push_back(root_lit);

    int qhead = start_trail;
    bool conflict = false;

    while (qhead < static_cast<int>(trail.size()) && !conflict) {
        int p = trail[qhead++];
        const auto& occ = (p > 0) ? neg_occ[p] : pos_occ[-p];

        for (int c_idx : occ) {
            const auto& cl = clauses[c_idx];
            bool sat = false;
            int unassigned_lit = 0;
            int unassigned_count = 0;

            for (int lit : cl) {
                int var = std::abs(lit);
                int val = assigns[var];
                if (val != 0) {
                    if ((val == 1 && lit > 0) || (val == -1 && lit < 0)) {
                        sat = true;
                        break;
                    }
                } else {
                    unassigned_count++;
                    unassigned_lit = lit;
                    if (unassigned_count > 1) break;
                }
            }

            if (sat) continue;

            if (unassigned_count == 0) {
                conflict = true;
                break;
            } else if (unassigned_count == 1) {
                int uvar = std::abs(unassigned_lit);
                assigns[uvar] = (unassigned_lit > 0 ? 1 : -1);
                trail.push_back(unassigned_lit);
            }
        }
    }

    if (conflict) {
        for (size_t i = start_trail; i < trail.size(); ++i) assigns[std::abs(trail[i])] = 0;
        trail.resize(start_trail);
        return -1;
    }

    int hop1_props = static_cast<int>(trail.size()) - start_trail;

    // Туннельный луч (2-й уровень зондирования сквозь стену):
    int best_neighbor = 0;
    int max_freq = 0;
    for (size_t i = start_trail; i < trail.size(); ++i) {
        int v = std::abs(trail[i]);
        const auto& occ = (trail[i] > 0) ? pos_occ[v] : neg_occ[v];
        for (int cid : occ) {
            for (int l : clauses[cid]) {
                int nv = std::abs(l);
                if (assigns[nv] == 0 && var_freq[nv] > max_freq) {
                    max_freq = var_freq[nv];
                    best_neighbor = nv;
                }
            }
        }
    }

    int bonus_props = 0;
    if (best_neighbor > 0) {
        int p2 = probe_up_count(best_neighbor, pos_occ, neg_occ, clauses, assigns, trail);
        int n2 = probe_up_count(-best_neighbor, pos_occ, neg_occ, clauses, assigns, trail);
        if (p2 == -1 && n2 == -1) {
            // Обе ветви соседа приводят к тупику -> корень на самом деле UNSAT!
            for (size_t i = start_trail; i < trail.size(); ++i) assigns[std::abs(trail[i])] = 0;
            trail.resize(start_trail);
            return -1;
        }
        bonus_props = std::max(0, std::max(p2, n2));
    }

    for (size_t i = start_trail; i < trail.size(); ++i) assigns[std::abs(trail[i])] = 0;
    trail.resize(start_trail);

    return hop1_props + bonus_props;
}

std::vector<std::vector<int>> MarchCubingEngine::generate_cubes(
    int max_cubes,
    int max_depth,
    const std::vector<int>& priority_vars
) {
    auto t0 = std::chrono::high_resolution_clock::now();
    stats = MarchCubeStats();

    // 1. Построение списков вхождений
    std::vector<std::vector<int>> pos_occ(num_vars + 1);
    std::vector<std::vector<int>> neg_occ(num_vars + 1);
    std::vector<int> var_freq(num_vars + 1, 0);

    for (size_t i = 0; i < clauses.size(); ++i) {
        for (int lit : clauses[i]) {
            int v = std::abs(lit);
            if (v <= num_vars) {
                var_freq[v]++;
                if (lit > 0) pos_occ[v].push_back(static_cast<int>(i));
                else neg_occ[v].push_back(static_cast<int>(i));
            }
        }
    }

    // 2. Выбор лучших кандидатов для Lookahead
    std::vector<int> candidates;
    if (!priority_vars.empty()) {
        candidates = priority_vars;
    } else {
        std::vector<std::pair<int, int>> sorted_vars;
        sorted_vars.reserve(num_vars);
        for (int v = 1; v <= num_vars; ++v) {
            if (var_freq[v] >= 4) {
                sorted_vars.push_back({var_freq[v], v});
            }
        }
        std::sort(sorted_vars.rbegin(), sorted_vars.rend());
        int limit = std::min(static_cast<int>(sorted_vars.size()), 500);
        for (int i = 0; i < limit; ++i) {
            candidates.push_back(sorted_vars[i].second);
        }
    }

    // 3. Вычисление March Lookahead Score с туннельным лучом
    std::vector<int8_t> assigns(num_vars + 1, 0);
    std::vector<int> trail;
    trail.reserve(num_vars + 1);

    std::vector<VarScore> scored_vars;
    scored_vars.reserve(candidates.size());

    for (int v : candidates) {
        int p = probe_tunnel_up_count(v, pos_occ, neg_occ, clauses, assigns, trail, var_freq);
        int n = probe_tunnel_up_count(-v, pos_occ, neg_occ, clauses, assigns, trail, var_freq);

        double score = 0.0;
        if (p == -1 && n == -1) {
            score = -1.0;
        } else if (p == -1 || n == -1) {
            score = 1e8 + (p == -1 ? n : p);
        } else {
            score = static_cast<double>(p + 10) * static_cast<double>(n + 10) + var_freq[v] * 2.0;
        }

        scored_vars.push_back({v, score});
    }

    std::sort(scored_vars.begin(), scored_vars.end(), [](const VarScore& a, const VarScore& b) {
        return a.score > b.score;
    });

    int select_count = std::min(max_depth, static_cast<int>(scored_vars.size()));
    std::vector<int> pivot_vars;
    for (int i = 0; i < select_count; ++i) {
        pivot_vars.push_back(scored_vars[i].var);
    }
    stats.pivot_vars = pivot_vars;

    // 4. Построение сбалансированного бинарного дерева кубов
    std::vector<std::vector<int>> cubes;
    int target_tree_depth = 0;
    while ((1 << (target_tree_depth + 1)) <= max_cubes && target_tree_depth < static_cast<int>(pivot_vars.size())) {
        target_tree_depth++;
    }

    int total_leaves = 1 << target_tree_depth;
    cubes.reserve(total_leaves);

    for (int mask = 0; mask < total_leaves; ++mask) {
        std::vector<int> cube;
        cube.reserve(target_tree_depth);
        for (int d = 0; d < target_tree_depth; ++d) {
            int v = pivot_vars[d];
            int bit = (mask >> d) & 1;
            cube.push_back(bit ? v : -v);
        }

        // Быстрая туннельная проверка куба на коллизию корня
        bool valid = true;
        for (int lit : cube) {
            int u = probe_tunnel_up_count(lit, pos_occ, neg_occ, clauses, assigns, trail, var_freq);
            if (u == -1) {
                valid = false;
                stats.pruned_branches++;
                break;
            }
        }

        if (valid) {
            cubes.push_back(std::move(cube));
        }
    }

    stats.generated_cubes = static_cast<int>(cubes.size());
    auto t1 = std::chrono::high_resolution_clock::now();
    stats.time_s = std::chrono::duration<double>(t1 - t0).count();

    return cubes;
}
