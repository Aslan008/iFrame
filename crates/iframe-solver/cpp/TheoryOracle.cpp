#include "TheoryOracle.h"
#include "ParallelSolver.h"
#include "Solver.h"
#include <cmath>
#include <iostream>
#include <queue>
#include <unordered_set>
#include <algorithm>
#ifdef _MSC_VER
#include <intrin.h>
#endif

// Вычисление НОД (GCD) для 64-битных целых
static inline uint64_t gcd_64(uint64_t a, uint64_t b) {
    while (b != 0) {
        uint64_t t = b;
        b = a % b;
        a = t;
    }
    return a;
}

MultiplierStructure TheoryOracle::analyze_multiplier(int num_vars, const std::vector<std::vector<int>>& clauses) {
    MultiplierStructure res;
    if (num_vars < 10 || clauses.empty()) return res;

    // 1. Поиск AND-вентилей: o <-> a & b
    // Клауза: (o, -a, -b)
    std::unordered_map<int, std::unordered_set<int>> adj;
    for (const auto& cl : clauses) {
        if (cl.size() == 3) {
            int pos_lit = 0;
            int neg1 = 0, neg2 = 0;
            for (int lit : cl) {
                if (lit > 0) pos_lit = lit;
                else if (neg1 == 0) neg1 = -lit;
                else neg2 = -lit;
            }
            if (pos_lit > 0 && neg1 > 0 && neg2 > 0) {
                adj[neg1].insert(neg2);
                adj[neg2].insert(neg1);
            }
        }
    }

    // 2. Ищем максимальный полный двудольный граф K_{n,n}
    // Кандидаты на размер n: ищем максимальную степень вершины
    int max_deg = 0;
    int best_pivot = 0;
    for (const auto& p : adj) {
        if (static_cast<int>(p.second.size()) > max_deg) {
            max_deg = static_cast<int>(p.second.size());
            best_pivot = p.first;
        }
    }

    if (max_deg < 4 || best_pivot == 0) return res;

    int n_bits = max_deg;
    std::vector<int> group_Q(adj[best_pivot].begin(), adj[best_pivot].end());
    std::sort(group_Q.begin(), group_Q.end());

    std::unordered_set<int> set_Q(group_Q.begin(), group_Q.end());
    std::vector<int> group_P;

    // Переменные P должны быть соединены со всеми вершинами из Q
    for (const auto& p : adj) {
        int v = p.first;
        if (set_Q.count(v)) continue;
        bool all_connected = true;
        for (int q_var : group_Q) {
            if (!p.second.count(q_var)) {
                all_connected = false;
                break;
            }
        }
        if (all_connected) {
            group_P.push_back(v);
        }
    }

    std::sort(group_P.begin(), group_P.end());

    if (static_cast<int>(group_P.size()) != n_bits || static_cast<int>(group_Q.size()) != n_bits) {
        return res;
    }

    std::unordered_set<int> seen_P(group_P.begin(), group_P.end());
    std::unordered_set<int> seen_Q(group_Q.begin(), group_Q.end());

    // 3. Извлечение фиксированных юнит-клозов для целевого числа N
    // В стандартном CNF выходы произведения N зафиксированы юнит-клозами в порядке бит k = 0..2n-1
    std::vector<std::pair<int, int>> out_units;
    for (const auto& cl : clauses) {
        if (cl.size() == 1) {
            int lit = cl[0];
            int v = std::abs(lit);
            if (!seen_P.count(v) && !seen_Q.count(v)) {
                out_units.push_back({v, (lit > 0 ? 1 : 0)});
            }
        }
    }

    uint64_t target_N = 0;
    for (size_t k = 0; k < out_units.size() && k < 64; ++k) {
        if (out_units[k].second == 1) {
            target_N |= (1ULL << k);
        }
    }

    if (target_N == 0) return res;

    res.valid = true;
    res.n_bits = n_bits;
    res.target_N = target_N;
    res.p_vars = group_P;
    res.q_vars = group_Q;
    for (const auto& ou : out_units) {
        res.out_vars.push_back(ou.first);
    }

    return res;
}

bool TheoryOracle::simulate_and_complete_model(int num_vars,
                                               const std::vector<std::vector<int>>& clauses,
                                               const std::vector<int>& p_vars,
                                               const std::vector<int>& q_vars,
                                               uint64_t P,
                                               uint64_t Q,
                                               std::vector<int>& out_assigns) {
    out_assigns.assign(num_vars + 1, 0);
    std::queue<int> q;

    // 1. Устанавливаем биты множителей P и Q
    for (size_t i = 0; i < p_vars.size(); ++i) {
        int v = p_vars[i];
        int val = ((P >> i) & 1ULL) ? 1 : -1;
        out_assigns[v] = val;
        q.push(val > 0 ? v : -v);
    }
    for (size_t i = 0; i < q_vars.size(); ++i) {
        int v = q_vars[i];
        int val = ((Q >> i) & 1ULL) ? 1 : -1;
        out_assigns[v] = val;
        q.push(val > 0 ? v : -v);
    }

    // 2. Устанавливаем юнит-клозы формулы
    for (const auto& cl : clauses) {
        if (cl.size() == 1) {
            int lit = cl[0];
            int v = std::abs(lit);
            int val = (lit > 0 ? 1 : -1);
            if (out_assigns[v] == 0) {
                out_assigns[v] = val;
                q.push(lit);
            }
        }
    }

    // 3. Полная имитация логической схемы через Unit Propagation
    // Создаем список смежности клозов для распространения
    std::vector<std::vector<int>> occ(num_vars + 1);
    for (size_t ci = 0; ci < clauses.size(); ++ci) {
        for (int lit : clauses[ci]) {
            occ[std::abs(lit)].push_back(static_cast<int>(ci));
        }
    }

    while (!q.empty()) {
        int p_lit = q.front();
        q.pop();

        int p_var = std::abs(p_lit);
        for (int ci : occ[p_var]) {
            const auto& cl = clauses[ci];
            bool satisfied = false;
            int unassigned_lit = 0;
            int unassigned_count = 0;

            for (int lit : cl) {
                int v = std::abs(lit);
                int a = out_assigns[v];
                if (a != 0) {
                    if ((a == 1 && lit > 0) || (a == -1 && lit < 0)) {
                        satisfied = true;
                        break;
                    }
                } else {
                    unassigned_count++;
                    unassigned_lit = lit;
                }
            }

            if (satisfied) continue;

            if (unassigned_count == 1) {
                int uv = std::abs(unassigned_lit);
                int uval = (unassigned_lit > 0 ? 1 : -1);
                out_assigns[uv] = uval;
                q.push(unassigned_lit);
            }
        }
    }

    // 4. Проверяем, что ВСЕ клозы формулы удовлетворены
    for (const auto& cl : clauses) {
        bool sat = false;
        for (int lit : cl) {
            int v = std::abs(lit);
            int a = out_assigns[v];
            if ((a == 1 && lit > 0) || (a == -1 && lit < 0)) {
                sat = true;
                break;
            }
        }
        if (!sat) {
            return false;
        }
    }

    // Дозаполняем оставшиеся свободные переменные (если есть) значением 1 (true)
    for (int v = 1; v <= num_vars; ++v) {
        if (out_assigns[v] == 0) {
            out_assigns[v] = 1;
        }
    }

    return true;
}

bool TheoryOracle::run_oracle(int num_vars,
                              const std::vector<std::vector<int>>& clauses,
                              SharedClausePool& pool,
                              std::atomic<bool>& stop_flag) {
    auto mult = analyze_multiplier(num_vars, clauses);
    std::cout << "[TheoryOracle] detected=" << mult.valid << " N=" << mult.target_N << " n_bits=" << mult.n_bits << std::endl;
    if (!mult.valid || mult.target_N <= 3) {
        return false;
    }

    uint64_t N = mult.target_N;

    // 1. Алгоритмы факторизации оракула (Fermat & Pollard-Rho) с полной проверкой модели
    uint64_t A0 = static_cast<uint64_t>(std::ceil(std::sqrt(static_cast<double>(N))));
    uint64_t max_fermat_steps = 50000000ULL; // 50 миллионов шагов

    for (uint64_t A = A0; A < A0 + max_fermat_steps; ++A) {
        if (stop_flag.load(std::memory_order_relaxed)) {
            return false;
        }

        uint64_t delta = A * A - N;
        uint64_t B = static_cast<uint64_t>(std::round(std::sqrt(static_cast<double>(delta))));
        if (B * B == delta) {
            uint64_t P = A - B;
            uint64_t Q = A + B;
            if (P > 1 && Q > 1 && P * Q == N) {
                std::vector<int> full_model;
                if (simulate_and_complete_model(num_vars, clauses, mult.p_vars, mult.q_vars, P, Q, full_model)) {
                    std::lock_guard<std::mutex> lock(pool.finish_mutex);
                    if (!pool.terminate_all.load(std::memory_order_relaxed)) {
                        pool.winner_result.store(SolveResult::SAT, std::memory_order_relaxed);
                        pool.winner_assigns = std::move(full_model);
                        pool.winning_thread = 99; // Theory Oracle
                        pool.terminate_all.store(true, std::memory_order_relaxed);
                        return true;
                    }
                }
            }
        }
    }

    // 3. Алгоритм Полларда (Pollard's Rho) для чисел с удаленными множителями
    uint64_t x = 2, y = 2, d = 1, c = 1;
    uint64_t pollard_steps = 0;

    auto f_poly = [N, c](uint64_t val) -> uint64_t {
        // Умножение без переполнения для 64-бит
        #if defined(_MSC_VER) && defined(_M_X64)
        unsigned __int64 high;
        unsigned __int64 low = _umul128(val, val, &high);
        unsigned __int64 rem;
        _udiv128(high, low, N, &rem);
        return (rem + c) % N;
        #else
        unsigned __int128 v = val;
        return static_cast<uint64_t>((v * v + c) % N);
        #endif
    };

    while (d == 1 && pollard_steps < 20000000ULL) {
        if (stop_flag.load(std::memory_order_relaxed)) return false;

        x = f_poly(x);
        y = f_poly(f_poly(y));
        uint64_t diff = (x > y) ? (x - y) : (y - x);
        d = gcd_64(diff, N);
        pollard_steps++;

        if (d > 1 && d < N) {
            uint64_t P = d;
            uint64_t Q = N / d;
            std::vector<int> full_model;
            if (simulate_and_complete_model(num_vars, clauses, mult.p_vars, mult.q_vars, P, Q, full_model)) {
                std::lock_guard<std::mutex> lock(pool.finish_mutex);
                if (!pool.terminate_all.load(std::memory_order_relaxed)) {
                    pool.winner_result.store(SolveResult::SAT, std::memory_order_relaxed);
                    pool.winner_assigns = std::move(full_model);
                    pool.winning_thread = 99; // Theory Oracle
                    pool.terminate_all.store(true, std::memory_order_relaxed);
                    return true;
                }
            }
        }

        if (d == N) {
            // Перезапуск с новым seed
            x = (x + 3) % N;
            y = x;
            c++;
            d = 1;
        }
    }

    return false;
}
