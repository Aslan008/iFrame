#include "Gauss.h"
#include "Solver.h"
#include <map>
#include <set>

GaussEngine::GaussEngine(int num_vars)
    : num_vars(num_vars), num_words((num_vars + 63) / 64) {}

void GaussEngine::add_xor_clause(const std::vector<int>& vars, bool rhs) {
    XorRow row(num_words, rhs);
    for (int v : vars) {
        row.set_bit(v);
    }
    if (!row.is_empty()) {
        rows.push_back(std::move(row));
    }
}

int GaussEngine::extract_xors_from_clauses(const std::vector<CDCLClause*>& clauses) {
    // Группируем клозы по отсортированному набору переменных
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
        if (len < 2 || len > 5) continue; // поддерживаем размерности XOR от 2 до 5

        VarKey key;
        key.vars.reserve(len);
        for (int lit : c->lits) {
            key.vars.push_back(std::abs(lit));
        }
        std::sort(key.vars.begin(), key.vars.end());

        // Проверяем на дубликаты переменных в клозе
        bool has_dup = false;
        for (size_t i = 1; i < key.vars.size(); ++i) {
            if (key.vars[i] == key.vars[i - 1]) {
                has_dup = true;
                break;
            }
        }
        if (has_dup) continue;

        // Вычисляем битовую маску отрицаний в клозе
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

    int found_xors = 0;

    for (auto& entry : groups) {
        const auto& vars = entry.first.vars;
        int k = static_cast<int>(vars.size());
        auto& masks = entry.second;

        // Удаляем дубликаты масок
        std::sort(masks.begin(), masks.end());
        masks.erase(std::unique(masks.begin(), masks.end()), masks.end());

        int req_count = 1 << (k - 1);
        if (static_cast<int>(masks.size()) < req_count) continue;

        // Проверяем четную четность (even popcount) => XOR = 1
        bool all_even = true;
        int count_even = 0;
        for (uint32_t m : masks) {
#if defined(_MSC_VER)
            int pc = static_cast<int>(__popcnt(m));
#else
            int pc = __builtin_popcount(m);
#endif
            if ((pc % 2) == 0) {
                count_even++;
            } else {
                all_even = false;
            }
        }

        if (count_even == req_count) {
            add_xor_clause(vars, true); // Parity 1
            found_xors++;
            continue;
        }

        // Проверяем нечетную четность (odd popcount) => XOR = 0
        int count_odd = 0;
        for (uint32_t m : masks) {
#if defined(_MSC_VER)
            int pc = static_cast<int>(__popcnt(m));
#else
            int pc = __builtin_popcount(m);
#endif
            if ((pc % 2) != 0) {
                count_odd++;
            }
        }

        if (count_odd == req_count) {
            add_xor_clause(vars, false); // Parity 0
            found_xors++;
            continue;
        }
    }

    return found_xors;
}

bool GaussEngine::eliminate(std::vector<int>& var_alias,
                            std::vector<std::pair<int, int>>& level0_units,
                            int& out_substituted) {
    if (rows.empty()) return true;

    size_t pivot_row = 0;
    for (int col = 1; col <= num_vars && pivot_row < rows.size(); ++col) {
        size_t sel = pivot_row;
        while (sel < rows.size() && !rows[sel].get_bit(col)) {
            sel++;
        }
        if (sel == rows.size()) {
            continue;
        }

        std::swap(rows[pivot_row], rows[sel]);

        // Полный RREF (исключаем переменную col из всех остальных строк)
        for (size_t i = 0; i < rows.size(); ++i) {
            if (i != pivot_row && rows[i].get_bit(col)) {
                rows[i].xor_with(rows[pivot_row]);
            }
        }
        pivot_row++;
    }

    std::vector<XorRow> surviving_rows;
    surviving_rows.reserve(rows.size());

    for (const auto& row : rows) {
        int cnt = row.popcount();
        if (cnt == 0) {
            if (row.rhs) {
                // 0 = 1 => Доказано строгое противоречие (UNSAT)
                return false;
            }
            continue; // 0 = 0 (тривиальная строка)
        }

        if (cnt == 1) {
            // Унарное равенство: x_i = rhs
            int var = row.leading_var();
            int val = row.rhs ? 1 : -1;
            level0_units.push_back({var, val});
        } else if (cnt == 2) {
            // Бинарная эквивалентность: x_i ^ x_j = rhs => x_j = x_i ^ rhs
            int v1 = row.leading_var();
            int v2 = 0;
            for (int v = v1 + 1; v <= num_vars; ++v) {
                if (row.get_bit(v)) {
                    v2 = v;
                    break;
                }
            }
            if (v2 > 0) {
                int target_lit = row.rhs ? -v1 : v1;
                if (var_alias[v2] == 0) {
                    var_alias[v2] = target_lit;
                    out_substituted++;
                }
            }
        }

        surviving_rows.push_back(row);
    }

    rows = std::move(surviving_rows);
    return true;
}
