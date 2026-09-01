#include "XOREngine.h"
#include "Solver.h"
#include <map>
#if defined(_MSC_VER)
#include <intrin.h>
static inline int popcount32(uint32_t x) {
    return static_cast<int>(__popcnt(x));
}
#else
static inline int popcount32(uint32_t x) {
    return __builtin_popcount(x);
}
#endif

XOREngine::XOREngine(int num_vars) : num_vars(num_vars) {
    if (num_vars > 0) {
        init(num_vars);
    }
}

XOREngine::~XOREngine() {
    clear_reason_pool();
}

void XOREngine::init(int n) {
    num_vars = n;
    equations.clear();
    xor_watches.assign(num_vars + 1, std::vector<int>());
}

int XOREngine::extract_from_clauses(const std::vector<CDCLClause*>& clauses) {
    int max_v = num_vars;
    for (const auto* c : clauses) {
        if (c->deleted) continue;
        for (int lit : c->lits) {
            max_v = std::max(max_v, std::abs(lit));
        }
    }
    num_vars = max_v;

    equations.clear();
    xor_watches.assign(num_vars + 1, std::vector<int>());

    struct VarKey {
        std::vector<int> vars;
        bool operator<(const VarKey& o) const {
            return vars < o.vars;
        }
    };

    std::map<VarKey, std::vector<uint32_t>> groups;

    for (const auto* c : clauses) {
        if (c->deleted) continue;
        int len = static_cast<int>(c->lits.size());
        if (len < 2 || len > 5) continue; // размерности XOR от 2 до 5

        VarKey key;
        key.vars.reserve(len);
        for (int lit : c->lits) {
            key.vars.push_back(std::abs(lit));
        }
        std::sort(key.vars.begin(), key.vars.end());

        bool has_dup = false;
        for (size_t i = 1; i < key.vars.size(); ++i) {
            if (key.vars[i] == key.vars[i - 1]) {
                has_dup = true;
                break;
            }
        }
        if (has_dup) continue;

        uint32_t sign_mask = 0;
        for (int lit : c->lits) {
            int v = std::abs(lit);
            auto it = std::lower_bound(key.vars.begin(), key.vars.end(), v);
            int idx = static_cast<int>(std::distance(key.vars.begin(), it));
            if (lit < 0) {
                sign_mask |= (1U << idx);
            }
        }

        groups[key].push_back(sign_mask);
    }

    for (auto& entry : groups) {
        const auto& vars = entry.first.vars;
        int k = static_cast<int>(vars.size());
        auto& masks = entry.second;

        std::sort(masks.begin(), masks.end());
        masks.erase(std::unique(masks.begin(), masks.end()), masks.end());

        int req_count = 1 << (k - 1);
        if (static_cast<int>(masks.size()) < req_count) continue;

        int count_even = 0;
        for (uint32_t m : masks) {
            if ((popcount32(m) % 2) == 0) {
                count_even++;
            }
        }

        bool rhs = false;
        if (count_even == req_count) {
            rhs = true; // x1 ^ ... ^ xk = 1
        } else if (count_even == 0 && static_cast<int>(masks.size()) == req_count) {
            rhs = false; // x1 ^ ... ^ xk = 0
        } else {
            continue;
        }

        int eq_idx = static_cast<int>(equations.size());
        equations.push_back({vars, rhs});

        for (int v : vars) {
            if (v <= num_vars) {
                xor_watches[v].push_back(eq_idx);
            }
        }
    }

    return static_cast<int>(equations.size());
}

CDCLClause* XOREngine::propagate_var(int assigned_var,
                                     const std::vector<int>& assigns,
                                     std::vector<std::pair<int, CDCLClause*>>& to_assign) {
    if (assigned_var <= 0 || assigned_var > num_vars) return nullptr;

    const auto& watching_eqs = xor_watches[assigned_var];
    for (int eq_idx : watching_eqs) {
        const auto& eq = equations[eq_idx];

        int unassigned_count = 0;
        int last_unassigned_var = 0;
        bool current_parity = false;

        // Ленивый скан ≤ 5 переменных: 100% устойчивость к бэктрекингу!
        for (int v : eq.vars) {
            int val = assigns[v];
            if (val == 0) {
                unassigned_count++;
                last_unassigned_var = v;
            } else if (val == 1) {
                current_parity ^= true;
            }
        }

        if (unassigned_count == 0) {
            if (current_parity != eq.rhs) {
                // 1. XOR Конфликт (U = 0, Parity mismatch)
                std::vector<int> conflict_lits;
                conflict_lits.reserve(eq.vars.size());
                for (int v : eq.vars) {
                    conflict_lits.push_back(assigns[v] == 1 ? -v : v);
                }
                CDCLClause* conf_cl = new CDCLClause{conflict_lits, true, 1, false};
                reason_pool.push_back(conf_cl);
                return conf_cl;
            }
        } else if (unassigned_count == 1) {
            // 2. XOR Юнит-пропагация (U = 1)
            bool target_bool = eq.rhs ^ current_parity;
            int target_lit = target_bool ? last_unassigned_var : -last_unassigned_var;

            // Строим reason clause: [target_lit, ~falsified_lit1, ~falsified_lit2, ...]
            std::vector<int> reason_lits;
            reason_lits.reserve(eq.vars.size());
            reason_lits.push_back(target_lit);
            for (int v : eq.vars) {
                if (v != last_unassigned_var) {
                    reason_lits.push_back(assigns[v] == 1 ? -v : v);
                }
            }
            CDCLClause* reason_cl = new CDCLClause{reason_lits, true, 1, false};
            reason_pool.push_back(reason_cl);
            to_assign.push_back({target_lit, reason_cl});
        }
    }

    return nullptr;
}

void XOREngine::clear_reason_pool() {
    for (CDCLClause* c : reason_pool) {
        delete c;
    }
    reason_pool.clear();
}
