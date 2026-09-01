#include "AdderEngine.h"
#include "Solver.h"
#include "XOREngine.h"
#include <queue>
#include <set>
#include <unordered_set>
#include <unordered_map>

AdderEngine::AdderEngine(int num_vars) : num_vars(num_vars) {
    if (num_vars > 0) init(num_vars);
}

void AdderEngine::init(int nvars) {
    num_vars = nvars;
    full_adders.clear();
    half_adders.clear();
    var_to_fa.assign(num_vars + 1, {});
    var_to_ha.assign(num_vars + 1, {});
    bit_levels.assign(num_vars + 1, 0);
}

struct LitPairHash {
    size_t operator()(const std::pair<int, int>& p) const {
        return (static_cast<size_t>(p.first) * 1000003) ^ static_cast<size_t>(p.second);
    }
};

struct LitTripleHash {
    size_t operator()(const std::tuple<int, int, int>& t) const {
        size_t h1 = static_cast<size_t>(std::get<0>(t));
        size_t h2 = static_cast<size_t>(std::get<1>(t));
        size_t h3 = static_cast<size_t>(std::get<2>(t));
        return (h1 * 1000003) ^ (h2 * 1009) ^ h3;
    }
};

int AdderEngine::extract_adders(const std::vector<CDCLClause*>& clauses, const XOREngine& xor_engine) {
    full_adders.clear();
    half_adders.clear();

    int max_v = num_vars;
    for (CDCLClause* c : clauses) {
        if (c->deleted) continue;
        for (int lit : c->lits) {
            max_v = std::max(max_v, std::abs(lit));
        }
    }
    num_vars = max_v;

    var_to_fa.assign(num_vars + 1, {});
    var_to_ha.assign(num_vars + 1, {});
    bit_levels.assign(num_vars + 1, 0);

    // 1. Построение карты бинарных и тройных клозов
    std::unordered_set<std::pair<int, int>, LitPairHash> binary_clauses;
    std::unordered_map<std::tuple<int, int, int>, CDCLClause*, LitTripleHash> ternary_clauses;
    std::unordered_map<std::pair<int, int>, std::vector<int>, LitPairHash> pair_to_third;

    auto make_canonical_triple = [](int a, int b, int c) -> std::tuple<int, int, int> {
        int arr[3] = {a, b, c};
        std::sort(arr, arr + 3);
        return {arr[0], arr[1], arr[2]};
    };

    auto make_canonical_pair = [](int a, int b) -> std::pair<int, int> {
        if (a > b) std::swap(a, b);
        return {a, b};
    };

    for (CDCLClause* c : clauses) {
        if (c->deleted) continue;
        if (c->lits.size() == 2) {
            binary_clauses.insert(make_canonical_pair(c->lits[0], c->lits[1]));
        } else if (c->lits.size() == 3) {
            auto trip = make_canonical_triple(c->lits[0], c->lits[1], c->lits[2]);
            ternary_clauses[trip] = c;
            pair_to_third[{c->lits[0], c->lits[1]}].push_back(c->lits[2]);
            pair_to_third[{c->lits[0], c->lits[2]}].push_back(c->lits[1]);
            pair_to_third[{c->lits[1], c->lits[2]}].push_back(c->lits[0]);
        }
    }

    auto has_binary = [&](int a, int b) {
        return binary_clauses.find(make_canonical_pair(a, b)) != binary_clauses.end();
    };

    auto get_ternary = [&](int a, int b, int c) -> CDCLClause* {
        auto it = ternary_clauses.find(make_canonical_triple(a, b, c));
        if (it != ternary_clauses.end()) return it->second;
        return nullptr;
    };

    auto has_ternary = [&](int a, int b, int c) {
        return get_ternary(a, b, c) != nullptr;
    };

    const auto& xor_eqs = xor_engine.get_equations();
    for (size_t x_idx = 0; x_idx < xor_eqs.size(); ++x_idx) {
        const auto& eq = xor_eqs[x_idx];

        // Full Adder Sum: a ^ b ^ cin ^ sum = 0 (4 переменные)
        if (eq.vars.size() == 4 && !eq.rhs) {
            int vars_arr[4] = {eq.vars[0], eq.vars[1], eq.vars[2], eq.vars[3]};

            bool found_fa = false;
            for (int sum_idx = 0; sum_idx < 4 && !found_fa; ++sum_idx) {
                int sum_var = vars_arr[sum_idx];
                int a = vars_arr[(sum_idx + 1) % 4];
                int b = vars_arr[(sum_idx + 2) % 4];
                int cin = vars_arr[(sum_idx + 3) % 4];

                auto it_ab = pair_to_third.find({-a, -b});
                if (it_ab == pair_to_third.end()) continue;

                for (int cout_var : it_ab->second) {
                    if (cout_var <= 0 || cout_var == a || cout_var == b || cout_var == cin || cout_var == sum_var) continue;

                    // Строгая проверка всех 6 клозов мажоритарной функции carry
                    if (has_ternary(-a, -cin, cout_var) &&
                        has_ternary(-b, -cin, cout_var) &&
                        has_ternary(a, b, -cout_var) &&
                        has_ternary(a, cin, -cout_var) &&
                        has_ternary(b, cin, -cout_var)) {

                        CDCLClause* reason_cl = get_ternary(-a, -cin, cout_var);
                        FullAdder fa{a, b, cin, sum_var, cout_var, reason_cl};
                        int fa_idx = static_cast<int>(full_adders.size());
                        full_adders.push_back(fa);

                        var_to_fa[a].push_back(fa_idx);
                        var_to_fa[b].push_back(fa_idx);
                        var_to_fa[cin].push_back(fa_idx);
                        var_to_fa[sum_var].push_back(fa_idx);
                        var_to_fa[cout_var].push_back(fa_idx);

                        found_fa = true;
                        break;
                    }
                }
            }
        }
        // Half Adder Sum: a ^ b ^ sum = 0 (3 переменные)
        else if (eq.vars.size() == 3 && !eq.rhs) {
            int vars_arr[3] = {eq.vars[0], eq.vars[1], eq.vars[2]};

            bool found_ha = false;
            for (int sum_idx = 0; sum_idx < 3 && !found_ha; ++sum_idx) {
                int sum_var = vars_arr[sum_idx];
                int a = vars_arr[(sum_idx + 1) % 3];
                int b = vars_arr[(sum_idx + 2) % 3];

                auto it_ab = pair_to_third.find({-a, -b});
                if (it_ab == pair_to_third.end()) continue;

                for (int carry_var : it_ab->second) {
                    if (carry_var <= 0 || carry_var == a || carry_var == b || carry_var == sum_var) continue;

                    // Строгая проверка всех 3 клозов carry (AND): (~a | ~b | carry), (a | ~carry), (b | ~carry)
                    if (has_binary(a, -carry_var) && has_binary(b, -carry_var)) {
                        CDCLClause* reason_cl = (!clauses.empty()) ? clauses[0] : nullptr;
                        HalfAdder ha{a, b, sum_var, carry_var, reason_cl};
                        int ha_idx = static_cast<int>(half_adders.size());
                        half_adders.push_back(ha);

                        var_to_ha[a].push_back(ha_idx);
                        var_to_ha[b].push_back(ha_idx);
                        var_to_ha[sum_var].push_back(ha_idx);
                        var_to_ha[carry_var].push_back(ha_idx);

                        found_ha = true;
                        break;
                    }
                }
            }
        }
    }

    // 3. Топологическая сортировка DAG цепочек переносов (LSB -> MSB)
    std::vector<int> in_degree(num_vars + 1, 0);
    std::vector<std::vector<int>> carry_adj(num_vars + 1);

    for (const auto& fa : full_adders) {
        carry_adj[fa.a].push_back(fa.cout);
        carry_adj[fa.b].push_back(fa.cout);
        carry_adj[fa.cin].push_back(fa.cout);
        in_degree[fa.cout] += 3;
    }
    for (const auto& ha : half_adders) {
        carry_adj[ha.a].push_back(ha.carry);
        carry_adj[ha.b].push_back(ha.carry);
        in_degree[ha.carry] += 2;
    }

    std::queue<int> q;
    for (int v = 1; v <= num_vars; ++v) {
        if (in_degree[v] == 0) {
            bit_levels[v] = 0;
            q.push(v);
        }
    }

    while (!q.empty()) {
        int u = q.front();
        q.pop();

        for (int next_var : carry_adj[u]) {
            bit_levels[next_var] = std::max(bit_levels[next_var], bit_levels[u] + 1);
            if (--in_degree[next_var] == 0) {
                q.push(next_var);
            }
        }
    }

    return static_cast<int>(full_adders.size() + half_adders.size());
}

CDCLClause* AdderEngine::propagate_var(
    int var,
    const std::vector<int>& assigns,
    std::vector<std::pair<int, CDCLClause*>>& out_units
) {
    if (var < 1 || var >= static_cast<int>(var_to_fa.size())) return nullptr;

    // 1. Проверяем Full Adders, связанные с этой переменной
    for (int fa_idx : var_to_fa[var]) {
        const auto& fa = full_adders[fa_idx];
        int val_a = assigns[fa.a];     // -1 (false), 0 (undef), 1 (true)
        int val_b = assigns[fa.b];
        int val_cin = assigns[fa.cin];
        int val_sum = assigns[fa.sum];
        int val_cout = assigns[fa.cout];

        int ones = (val_a == 1) + (val_b == 1) + (val_cin == 1);
        int zeros = (val_a == -1) + (val_b == -1) + (val_cin == -1);
        int undef_in = 3 - (ones + zeros);

        // A. Forward Carry: если 2 единицы -> cout = 1, если 2 нуля -> cout = 0
        if (ones >= 2) {
            if (val_cout == -1) return fa.reason_clause;
            if (val_cout == 0) out_units.push_back({fa.cout, fa.reason_clause});
        } else if (zeros >= 2) {
            if (val_cout == 1) return fa.reason_clause;
            if (val_cout == 0) out_units.push_back({-fa.cout, fa.reason_clause});
        }

        // B. Forward Sum: если все 3 входа известны -> sum = ones % 2
        if (undef_in == 0) {
            int expected_sum = (ones % 2 == 1) ? 1 : -1;
            if (val_sum != 0 && val_sum != expected_sum) return fa.reason_clause;
            if (val_sum == 0) out_units.push_back({expected_sum > 0 ? fa.sum : -fa.sum, fa.reason_clause});
        }

        // C. Backward Inversion: если известны sum и cout
        if (val_cout != 0 && val_sum != 0) {
            int cout_b = (val_cout == 1) ? 1 : 0;
            int sum_b = (val_sum == 1) ? 1 : 0;
            int total = 2 * cout_b + sum_b; // 0..3

            if (total == 0) { // Все входы должны быть 0 (-1)
                if (val_a == 1 || val_b == 1 || val_cin == 1) return fa.reason_clause;
                if (val_a == 0) out_units.push_back({-fa.a, fa.reason_clause});
                if (val_b == 0) out_units.push_back({-fa.b, fa.reason_clause});
                if (val_cin == 0) out_units.push_back({-fa.cin, fa.reason_clause});
            } else if (total == 3) { // Все входы должны быть 1 (+1)
                if (val_a == -1 || val_b == -1 || val_cin == -1) return fa.reason_clause;
                if (val_a == 0) out_units.push_back({fa.a, fa.reason_clause});
                if (val_b == 0) out_units.push_back({fa.b, fa.reason_clause});
                if (val_cin == 0) out_units.push_back({fa.cin, fa.reason_clause});
            } else if (total == 1) {
                if (ones > 1) return fa.reason_clause;
                if (ones == 1) {
                    if (val_a == 0) out_units.push_back({-fa.a, fa.reason_clause});
                    if (val_b == 0) out_units.push_back({-fa.b, fa.reason_clause});
                    if (val_cin == 0) out_units.push_back({-fa.cin, fa.reason_clause});
                } else if (zeros == 2) {
                    if (val_a == 0) out_units.push_back({fa.a, fa.reason_clause});
                    if (val_b == 0) out_units.push_back({fa.b, fa.reason_clause});
                    if (val_cin == 0) out_units.push_back({fa.cin, fa.reason_clause});
                }
            } else if (total == 2) {
                if (zeros > 1) return fa.reason_clause;
                if (zeros == 1) {
                    if (val_a == 0) out_units.push_back({fa.a, fa.reason_clause});
                    if (val_b == 0) out_units.push_back({fa.b, fa.reason_clause});
                    if (val_cin == 0) out_units.push_back({fa.cin, fa.reason_clause});
                } else if (ones == 2) {
                    if (val_a == 0) out_units.push_back({-fa.a, fa.reason_clause});
                    if (val_b == 0) out_units.push_back({-fa.b, fa.reason_clause});
                    if (val_cin == 0) out_units.push_back({-fa.cin, fa.reason_clause});
                }
            }
        }
    }

    // 2. Проверяем Half Adders
    for (int ha_idx : var_to_ha[var]) {
        const auto& ha = half_adders[ha_idx];
        int val_a = assigns[ha.a];
        int val_b = assigns[ha.b];
        int val_sum = assigns[ha.sum];
        int val_carry = assigns[ha.carry];

        if (val_carry == 1) {
            if (val_a == -1 || val_b == -1 || val_sum == 1) return ha.reason_clause;
            if (val_a == 0) out_units.push_back({ha.a, ha.reason_clause});
            if (val_b == 0) out_units.push_back({ha.b, ha.reason_clause});
            if (val_sum == 0) out_units.push_back({-ha.sum, ha.reason_clause});
        } else if (val_carry == -1) {
            if (val_a == 1) {
                if (val_b == 1) return ha.reason_clause;
                if (val_b == 0) out_units.push_back({-ha.b, ha.reason_clause});
            }
            if (val_b == 1) {
                if (val_a == 1) return ha.reason_clause;
                if (val_a == 0) out_units.push_back({-ha.a, ha.reason_clause});
            }
            if (val_sum == -1) {
                if (val_a == 1 || val_b == 1) return ha.reason_clause;
                if (val_a == 0) out_units.push_back({-ha.a, ha.reason_clause});
                if (val_b == 0) out_units.push_back({-ha.b, ha.reason_clause});
            }
        }
    }

    return nullptr;
}
