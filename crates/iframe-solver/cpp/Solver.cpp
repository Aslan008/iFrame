#include "Solver.h"
#include "ParallelSolver.h"
#include "BIG.h"
#include <algorithm>
#include <unordered_set>
#include <chrono>
#include <cmath>

CDCLSolver::CDCLSolver(int num_vars, const std::vector<std::vector<int>>& initial_clauses, int max_conflicts, int sim_steps, int physics_max_lbd, int race_every, int dim)
    : num_vars(num_vars), max_conflicts(max_conflicts), sim_steps(sim_steps), physics_max_lbd(physics_max_lbd), race_every(race_every), dim(dim),
      next_reduce_conflict(2000),
      reduce_increment(500),
      ok(true), conflicts(0), decisions_count(0), restarts(0), qhead(0), sim(num_vars, dim), conflicts_since_restart(0), restart_limit(100),
      propagations(0), cache_hits(0), physics_time(0.0), race_cache_pos(0), race_uses(0),
      activity(num_vars + 1, 0.0), var_inc(1.0)
{
    assigns.resize(num_vars + 1, 0);
    decision_level.resize(num_vars + 1, 0);
    reason.resize(num_vars + 1, nullptr);
    saved_phases.resize(num_vars + 1, 1);
    target_phases.resize(num_vars + 1, 1);
    saved_positions.resize(num_vars + 1, (dim == 1) ? 0.5 : 0.0);
    watches.resize(2 * (num_vars + 1));
    seen.resize(num_vars + 1, false);
    seen_level.resize(num_vars + 1, 0);
    var_alias.resize(num_vars + 1, 0);
    is_eliminated.resize(num_vars + 1, false);
    
    // Преаллокация, чтобы избежать реаллокаций при вставке
    clauses.reserve(initial_clauses.size() * 2);
    original_clauses.reserve(initial_clauses.size());
    learned_clauses.reserve(max_conflicts);
    
    for (const auto& c : initial_clauses) {
        CDCLClause* cl = alloc_clause(c, false, 0);
        
        clauses.push_back(cl);
        original_clauses.push_back(cl);
        
        if (cl->lits.size() >= 2) {
            attach_clause(cl);
        } else if (cl->lits.size() == 1) {
            int lit = cl->lits[0];
            int val = lit_val(lit);
            if (val == -1) ok = false;
            else if (val == 0) assign_lit(lit, cl);
        } else {
            ok = false;
        }
    }
}

CDCLSolver::CDCLSolver(int num_vars, const std::vector<std::vector<int>>& initial_clauses, const std::vector<int>& assumptions, int max_conflicts, int sim_steps,
                       int physics_max_lbd, int race_every, int dim)
    : num_vars(num_vars), max_conflicts(max_conflicts), sim_steps(sim_steps),
      conflicts_since_restart(0), restart_limit(100),
      physics_max_lbd(physics_max_lbd), race_every(race_every), dim(dim),
      next_reduce_conflict(2000), reduce_increment(300),
      conflicts(0), decisions_count(0), restarts(0), ok(true),
      propagations(0), cache_hits(0), physics_time(0.0),
      race_cache_pos(0), race_uses(0),
      sim(num_vars, dim),
      activity(num_vars + 1, 0.0), var_inc(1.0)
{
    assigns.resize(num_vars + 1, 0);
    decision_level.resize(num_vars + 1, 0);
    reason.resize(num_vars + 1, nullptr);
    saved_phases.resize(num_vars + 1, 1);
    target_phases.resize(num_vars + 1, 1);
    saved_positions.resize(num_vars + 1, (dim == 1) ? 0.5 : 0.0);
    watches.resize(2 * (num_vars + 1));
    seen.resize(num_vars + 1, false);
    seen_level.resize(num_vars + 1, 0);
    var_alias.resize(num_vars + 1, 0);
    is_eliminated.resize(num_vars + 1, false);
    
    clauses.reserve(initial_clauses.size() + assumptions.size() + 100);
    original_clauses.reserve(initial_clauses.size() + assumptions.size());
    learned_clauses.reserve(max_conflicts);
    
    for (const auto& c : initial_clauses) {
        CDCLClause* cl = alloc_clause(c, false, 0);
        clauses.push_back(cl);
        original_clauses.push_back(cl);
        
        if (cl->lits.size() >= 2) {
            attach_clause(cl);
        } else if (cl->lits.size() == 1) {
            int lit = cl->lits[0];
            int val = lit_val(lit);
            if (val == -1) ok = false;
            else if (val == 0) assign_lit(lit, cl);
        } else {
            ok = false;
        }
    }

    for (int lit : assumptions) {
        CDCLClause* cl = alloc_clause({lit}, false, 0);
        clauses.push_back(cl);
        original_clauses.push_back(cl);
        int val = lit_val(lit);
        if (val == -1) ok = false;
        else if (val == 0) assign_lit(lit, cl);
    }
}

CDCLSolver::~CDCLSolver() {
    for (auto c : clauses) {
        delete c;
    }
    for (auto c : clause_pool) {
        delete c;
    }
}

CDCLClause* CDCLSolver::alloc_clause(const std::vector<int>& lits, bool learned, int lbd) {
    CDCLClause* c = nullptr;
    if (!clause_pool.empty()) {
        c = clause_pool.back();
        clause_pool.pop_back();
        c->lits = lits;
        c->learned = learned;
        c->lbd = lbd;
        c->deleted = false;
    } else {
        c = new CDCLClause{lits, learned, lbd, false};
    }
    return c;
}

void CDCLSolver::free_clause(CDCLClause* c) {
    if (clause_pool.size() < 50000) {
        c->deleted = true;
        c->lits.clear();
        clause_pool.push_back(c);
    } else {
        delete c;
    }
}

int CDCLSolver::find_root_lit(int lit) {
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
    // Сжатие пути (path compression)
    if (curr != var) {
        var_alias[var] = acc_sign * curr;
    }
    return sign * acc_sign * curr;
}

bool CDCLSolver::apply_substitutions() {
    if (current_level() != 0) {
        backjump(0);
    }

    // 1. Очищаем все списки наблюдателей (2WL watches)
    for (auto& w : watches) {
        w.clear();
    }

    // 2. Перезаписываем литералы во всех клозах через канонические корни find_root_lit
    std::vector<CDCLClause*> new_clauses;
    new_clauses.reserve(clauses.size());

    for (CDCLClause* c : clauses) {
        if (c->deleted) continue;

        std::vector<int> simplified_lits;
        bool tautology = false;
        bool satisfied = false;

        for (int lit : c->lits) {
            int root = find_root_lit(lit);
            int v = lit_val(root);
            if (v == 1) {
                satisfied = true;
                break;
            } else if (v == -1) {
                // ложный литерал на уровне 0 — просто отбрасываем
                continue;
            }

            // Проверка на тавтологию и дубликаты
            bool dup = false;
            for (int prev : simplified_lits) {
                if (prev == root) {
                    dup = true;
                    break;
                }
                if (prev == -root) {
                    tautology = true;
                    break;
                }
            }
            if (tautology) break;
            if (!dup) simplified_lits.push_back(root);
        }

        if (satisfied || tautology) {
            c->deleted = true;
            free_clause(c);
            continue;
        }

        if (simplified_lits.empty()) {
            // Пустая клауза на уровне 0 => UNSAT
            ok = false;
            free_clause(c);
            clauses = std::move(new_clauses);
            return false;
        }

        c->lits = std::move(simplified_lits);

        if (c->lits.size() == 1) {
            int u_lit = c->lits[0];
            int u_val = lit_val(u_lit);
            if (u_val == 0) {
                assign_lit(u_lit, nullptr);
            } else if (u_val == -1) {
                ok = false;
                free_clause(c);
                clauses = std::move(new_clauses);
                return false;
            }
            c->deleted = true;
            free_clause(c);
        } else {
            attach_clause(c);
            new_clauses.push_back(c);
        }
    }

    clauses = std::move(new_clauses);

    // Очищаем списки от удаленных указателей для предотвращения висячих ссылок
    learned_clauses.erase(
        std::remove_if(learned_clauses.begin(), learned_clauses.end(), [](CDCLClause* cl) { return cl->deleted; }),
        learned_clauses.end()
    );
    original_clauses.erase(
        std::remove_if(original_clauses.begin(), original_clauses.end(), [](CDCLClause* cl) { return cl->deleted; }),
        original_clauses.end()
    );

    // 3. Запускаем BCP на уровне 0
    auto conflict = propagate();
    if (conflict != nullptr) {
        ok = false;
        return false;
    }

    return true;
}

void CDCLSolver::reconstruct_model() {
    // 1. Сначала восстанавливаем алиасы переменных из var_alias для всех уже назначенных корней
    for (int v = 1; v <= num_vars; ++v) {
        if (var_alias[v] != 0) {
            int root_lit = find_root_lit(v);
            int root_var = std::abs(root_lit);
            int root_val = assigns[root_var];
            if (root_val != 0) {
                int s = (root_lit > 0) ? 1 : -1;
                assigns[v] = s * root_val;
            }
        }
    }

    // 2. Восстанавливаем переменные, элиминированные BVE (по теореме SatElite)
    for (auto it = bve_stack.rbegin(); it != bve_stack.rend(); ++it) {
        int v = it->var;
        bool all_pos_satisfied = true;

        for (const auto& cl : it->pos_clauses) {
            bool clause_sat = false;
            for (int lit : cl) {
                int var = std::abs(lit);
                if (var == v) continue;
                int val = assigns[var];
                if (val == 0 && var_alias[var] != 0) {
                    int r = find_root_lit(var);
                    val = (r > 0 ? 1 : -1) * assigns[std::abs(r)];
                }
                if ((lit > 0 && val == 1) || (lit < 0 && val == -1)) {
                    clause_sat = true;
                    break;
                }
            }
            if (!clause_sat) {
                all_pos_satisfied = false;
                break;
            }
        }

        assigns[v] = all_pos_satisfied ? -1 : 1;
    }

    // 3. Финальный проход по var_alias, если какие-то алиасы ссылались на BVE-переменные
    for (int v = 1; v <= num_vars; ++v) {
        if (var_alias[v] != 0) {
            int root_lit = find_root_lit(v);
            int root_var = std::abs(root_lit);
            int root_val = assigns[root_var];
            if (root_val != 0) {
                int s = (root_lit > 0) ? 1 : -1;
                assigns[v] = s * root_val;
            }
        }
    }
}

bool CDCLSolver::run_bve_preprocessing() {
    if (!ok) return false;

    // 1. Помечаем все переменные, входящие в XOR-системы, как НЕПРИКОСНОВЕННЫЕ
    std::vector<bool> is_xor_var(num_vars + 1, false);
    for (const auto& eq : xor_engine.get_equations()) {
        for (int v : eq.vars) {
            if (v <= num_vars) is_xor_var[v] = true;
        }
    }

    // 2. Строим списки вхождений для исходных клозов
    std::vector<std::vector<CDCLClause*>> pos_occ(num_vars + 1);
    std::vector<std::vector<CDCLClause*>> neg_occ(num_vars + 1);

    for (CDCLClause* c : original_clauses) {
        if (c->deleted) continue;
        for (int lit : c->lits) {
            int v = std::abs(lit);
            if (lit > 0) pos_occ[v].push_back(c);
            else neg_occ[v].push_back(c);
        }
    }

    // 3. Отбираем кандидатов на элиминацию
    std::vector<int> candidates;
    candidates.reserve(num_vars);
    for (int v = 1; v <= num_vars; ++v) {
        if (is_xor_var[v] || var_alias[v] != 0 || assigns[v] != 0 || is_eliminated[v]) continue;
        candidates.push_back(v);
    }

    std::sort(candidates.begin(), candidates.end(), [&](int a, int b) {
        return (pos_occ[a].size() * neg_occ[a].size()) < (pos_occ[b].size() * neg_occ[b].size());
    });

    for (int v : candidates) {
        if (is_xor_var[v] || var_alias[v] != 0 || assigns[v] != 0 || is_eliminated[v]) continue;

        auto& p_list = pos_occ[v];
        auto& n_list = neg_occ[v];
        p_list.erase(std::remove_if(p_list.begin(), p_list.end(), [](CDCLClause* c) { return c->deleted; }), p_list.end());
        n_list.erase(std::remove_if(n_list.begin(), n_list.end(), [](CDCLClause* c) { return c->deleted; }), n_list.end());

        // Pure literal elimination (все положительные)
        if (n_list.empty()) {
            std::vector<std::vector<int>> pos_cls;
            for (auto c : p_list) {
                pos_cls.push_back(c->lits);
                detach_clause(c);
                c->deleted = true;
            }
            bve_stack.push_back({v, std::move(pos_cls)});
            is_eliminated[v] = true;
            bve_eliminated_vars++;
            continue;
        }

        // Pure literal elimination (все отрицательные)
        if (p_list.empty()) {
            for (auto c : n_list) {
                detach_clause(c);
                c->deleted = true;
            }
            bve_stack.push_back({v, {}});
            is_eliminated[v] = true;
            bve_eliminated_vars++;
            continue;
        }

        // Ограничение на размер декартова произведения
        if (p_list.size() * n_list.size() > 64) continue;

        std::vector<std::vector<int>> resolvents;
        resolvents.reserve(p_list.size() * n_list.size());

        for (CDCLClause* cp : p_list) {
            for (CDCLClause* cn : n_list) {
                std::vector<int> res;
                res.reserve(cp->lits.size() + cn->lits.size() - 2);
                bool tautology = false;

                for (int l : cp->lits) {
                    if (l == v) continue;
                    res.push_back(l);
                }
                for (int l : cn->lits) {
                    if (l == -v) continue;
                    for (int existing : res) {
                        if (existing == -l) {
                            tautology = true;
                            break;
                        }
                    }
                    if (tautology) break;
                    res.push_back(l);
                }

                if (!tautology) {
                    std::sort(res.begin(), res.end());
                    res.erase(std::unique(res.begin(), res.end()), res.end());
                    resolvents.push_back(std::move(res));
                }
            }
        }

        // Условие Boundedness: количество клозов не должно расти (Non-growth)
        if (resolvents.size() <= p_list.size() + n_list.size()) {
            std::vector<std::vector<int>> pos_cls;
            for (auto c : p_list) {
                pos_cls.push_back(c->lits);
                detach_clause(c);
                c->deleted = true;
            }
            for (auto c : n_list) {
                detach_clause(c);
                c->deleted = true;
            }

            bve_stack.push_back({v, std::move(pos_cls)});
            is_eliminated[v] = true;
            bve_eliminated_vars++;

            for (auto& r_lits : resolvents) {
                if (r_lits.empty()) {
                    ok = false;
                    return false;
                }
                if (r_lits.size() == 1) {
                    int val = lit_val(r_lits[0]);
                    if (val == -1) {
                        ok = false;
                        return false; // Доказанный UNSAT: резольвента противоречит уровню 0
                    } else if (val == 0) {
                        assign_lit(r_lits[0], nullptr);
                        if (propagate() != nullptr) {
                            ok = false;
                            return false;
                        }
                    }
                } else {
                    CDCLClause* nc = alloc_clause(r_lits, false, static_cast<int>(r_lits.size()));
                    clauses.push_back(nc);
                    original_clauses.push_back(nc);
                    attach_clause(nc);

                    for (int lit : nc->lits) {
                        int var = std::abs(lit);
                        if (lit > 0) pos_occ[var].push_back(nc);
                        else neg_occ[var].push_back(nc);
                    }
                }
            }
        }
    }

    return ok;
}

bool CDCLSolver::run_big_inprocessing() {
    BinaryImplicationGraph big(num_vars);
    int binaries = big.extract_from_clauses(clauses);
    if (binaries == 0) return true;

    std::vector<int> level0_units;
    int equiv_count = 0;
    bool sat = big.find_sccs_and_substitute(var_alias, level0_units, equiv_count, nullptr);
    if (!sat) {
        ok = false;
        return false;
    }

    if (equiv_count > 0) {
        big_equivs_found += equiv_count;
        substituted_vars += equiv_count;
        if (!apply_substitutions()) {
            return false;
        }
    }

    return true;
}

bool CDCLSolver::run_gauss_inprocessing() {
    GaussEngine gauss_engine(num_vars);
    int xors = gauss_engine.extract_xors_from_clauses(clauses);
    if (xors == 0) return true;
    gauss_xors_found += xors;

    std::vector<std::pair<int, int>> level0_units;
    int substituted = 0;
    bool sat = gauss_engine.eliminate(var_alias, level0_units, substituted);
    if (!sat) {
        ok = false;
        return false;
    }

    gauss_equivs_found += substituted;
    substituted_vars += substituted;

    if (current_level() != 0) {
        backjump(0);
    }

    for (const auto& u : level0_units) {
        int var = u.first;
        int val = u.second;
        int current_v = assigns[var];
        if (current_v == 0) {
            int lit = (val == 1) ? var : -var;
            assign_lit(lit, nullptr);
            gauss_units_found++;
            if (shared_pool) {
                shared_pool->publish_unit(lit);
            }
        } else if (current_v != val) {
            ok = false;
            return false;
        }
    }

    if (substituted > 0 || !level0_units.empty()) {
        if (!apply_substitutions()) {
            return false;
        }
    }

    return true;
}

void CDCLSolver::initialize_spectral_clusters() {
    var_cluster.assign(num_vars + 1, 0);
}

void CDCLSolver::attach_clause(CDCLClause* clause) {
    watches[lit_idx(clause->lits[0])].push_back(Watcher{clause, clause->lits[1]});
    watches[lit_idx(clause->lits[1])].push_back(Watcher{clause, clause->lits[0]});
}

void CDCLSolver::detach_clause(CDCLClause* clause) {
    if (clause->lits.size() < 2) return;
    int l0 = lit_idx(clause->lits[0]);
    int l1 = lit_idx(clause->lits[1]);
    if (l0 < static_cast<int>(watches.size())) {
        auto& ws0 = watches[l0];
        ws0.erase(std::remove_if(ws0.begin(), ws0.end(), [clause](const Watcher& w) { return w.clause == clause; }), ws0.end());
    }
    if (l1 < static_cast<int>(watches.size())) {
        auto& ws1 = watches[l1];
        ws1.erase(std::remove_if(ws1.begin(), ws1.end(), [clause](const Watcher& w) { return w.clause == clause; }), ws1.end());
    }
}



int CDCLSolver::current_level() {
    return static_cast<int>(trail_lim.size());
}

void CDCLSolver::new_decision_level() {
    trail_lim.push_back(static_cast<int>(trail.size()));
}

void CDCLSolver::assign_lit(int lit, CDCLClause* r) {
    int var = std::abs(lit);
    int val = (lit > 0) ? 1 : -1;
    assigns[var] = val;
    decision_level[var] = current_level();
    reason[var] = r;
    trail.push_back(lit);
    saved_phases[var] = (val == 1) ? 1 : 0;
    saved_positions[var] = (dim == 1) ? ((val == 1) ? 0.95 : 0.05) : ((val == 1) ? 0.95 : -0.95);
    evolution.record_propagation();
}

void CDCLSolver::backjump(int target_level) {
    while (static_cast<int>(trail_lim.size()) > target_level) {
        int level_start = trail_lim.back();
        trail_lim.pop_back();
        while (static_cast<int>(trail.size()) > level_start) {
            int lit = trail.back();
            trail.pop_back();
            int var = std::abs(lit);
            assigns[var] = 0;
            decision_level[var] = 0;
            reason[var] = nullptr;
        }
    }
    qhead = static_cast<int>(trail.size());
    if (target_level == 0 && shared_pool) {
        shared_pool->release_cube(thread_id);
    }
}

void CDCLSolver::vivify_clauses() {
    if (current_level() > 0) {
        backjump(0);
    }
    if (learned_clauses.empty()) return;

    std::vector<CDCLClause*> candidates;
    candidates.reserve(std::min(learned_clauses.size(), static_cast<size_t>(300)));
    for (auto c : learned_clauses) {
        if (!c->deleted && c->lbd >= 3 && c->lits.size() >= 3) {
            candidates.push_back(c);
            if (candidates.size() >= 300) break;
        }
    }
    if (candidates.empty()) return;

    long long budget = 50000;
    long long start_props = propagations;

    for (CDCLClause* c : candidates) {
        if (c->deleted || c->lits.size() < 3) continue;
        if ((propagations - start_props) > budget) break;

        // Отвязываем старые наблюдатели перед пробной пропагацией
        detach_clause(c);
        c->deleted = true;

        std::vector<int> orig_lits = c->lits;
        std::vector<int> new_lits;
        bool subsumed = false;
        bool conflict_found = false;

        for (size_t i = 0; i < orig_lits.size(); ++i) {
            int lit = orig_lits[i];
            int val = lit_val(lit);

            if (val == 1) {
                if (decision_level[std::abs(lit)] == 0) {
                    subsumed = true; // Истинно на уровне 0 -> клоз действительно сабсьюмлен формулой
                    break;
                } else {
                    // Истинно на уровне > 0 из-за пробных допущений -> текущий префикс new_lits достаточен
                    conflict_found = true;
                    break;
                }
            } else if (val == -1) {
                vivified_lits_removed++;
                continue;
            }

            new_decision_level();
            assign_lit(-lit, nullptr);
            new_lits.push_back(lit);

            CDCLClause* confl = propagate();
            if (confl != nullptr) {
                conflict_found = true;
                break;
            }
        }

        backjump(0);

        if (subsumed) {
            // Клоз истинен на уровне 0: остается c->deleted = true и detached
            continue;
        }

        if (conflict_found || new_lits.size() < orig_lits.size()) {
            if (new_lits.size() >= 2) {
                c->lits = std::move(new_lits);
                c->lbd = std::min(c->lbd, static_cast<int>(c->lits.size()));
                c->deleted = false;
                attach_clause(c);
                vivified_clauses++;
            } else if (new_lits.size() == 1) {
                int unit_lit = new_lits[0];
                int uval = lit_val(unit_lit);
                if (uval == -1) {
                    ok = false;
                    return; // Конфликт на уровне 0 -> UNSAT
                } else if (uval == 0) {
                    assign_lit(unit_lit, nullptr);
                    auto conf = propagate();
                    if (conf) {
                        ok = false;
                        return;
                    }
                }
                c->deleted = true;
                vivified_clauses++;
            } else if (new_lits.empty() && conflict_found) {
                // Конфликт на первом же допущении -> пустой клоз на уровне 0 -> UNSAT!
                ok = false;
                return;
            }
        } else {
            // Не сократился: возвращаем в строй и привязываем обратно
            c->deleted = false;
            attach_clause(c);
        }
    }
}

// Фаза 1 (soundness): пробная пропагация пары допущений на временном уровне
// с откатом (failed-literal probing). true <=> текущая база + {lit1, lit2}
// дают UP-конфликт, т.е. формула имплицирует ¬(lit1 ∧ lit2).
// Состояние поиска не меняется; saved_phases/positions могут получить шум от
// пробных назначений — это безвредно (только подсказка полярности).
bool CDCLSolver::up_conflict(int lit1, int lit2) {
    if (lit_val(lit1) == -1 || lit_val(lit2) == -1) return true; // Уже противоречиво на уровне 0
    int base_level = current_level();
    new_decision_level();
    if (lit_val(lit1) == 0) assign_lit(lit1, nullptr);
    if (lit_val(lit2) == 0) assign_lit(lit2, nullptr);
    bool conflict = (propagate() != nullptr);
    backjump(base_level);
    return conflict;
}

// Доказательство эквивалентности юнит-пропагацией:
//   same=true:   v1<->v2  <=> F∧{v1,¬v2} и F∧{¬v1,v2} обе противоречивы
//   same=false:  v1<->!v2 <=> F∧{v1,v2}  и F∧{¬v1,¬v2} обе противоречивы
// UP-конфликт строго доказывает невыполнимость допущений, поэтому true
// означает математически доказанную эквивалентность: влитые клозы
// имплицированы формулой и не могут ни отсечь модели, ни дать ложный UNSAT.
bool CDCLSolver::verify_equivalence(int v1, int v2, bool same) {
    if (same) {
        if (!up_conflict(v1, -v2)) return false;  // v1 -> v2
        return up_conflict(-v1, v2);              // v2 -> v1
    } else {
        if (!up_conflict(v1, v2)) return false;   // v1 -> !v2
        return up_conflict(-v1, -v2);             // !v1 -> v2
    }
}

// Тестовый вход для --equiv-test: уровень-0 пропагация + верификация пары.
bool CDCLSolver::debug_verify_equivalence(int v1, int v2, bool same) {
    if (!ok) return false;
    if (propagate() != nullptr) return false;   // формула противоречива на уровне 0
    if (v1 < 1 || v1 > num_vars || v2 < 1 || v2 > num_vars || v1 == v2) return false;
    return verify_equivalence(v1, v2, same);
}

CDCLClause* CDCLSolver::propagate() {
    while (qhead < static_cast<int>(trail.size())) {
        int p = trail[qhead++];
        propagations++;
        int false_lit = -p;
        int false_idx = lit_idx(false_lit);
        
        // In-place обновление watch-листа (MiniSat two-pointer с blocking literal):
        auto& ws = watches[false_idx];
        size_t i = 0, j = 0;
        CDCLClause* conflict = nullptr;
        
        for (; i < ws.size(); ++i) {
            Watcher w = ws[i];

            // 1. Быстрая проверка блокирующего литерала (L1-кэш, без разыменования клоза!)
            if (lit_val(w.blocker) == 1) {
                ws[j++] = w;
                continue;
            }

            auto clause = w.clause;
            if (clause->deleted) continue; // вычищаем удалённые из watch-листов
            
            if (clause->lits[0] == false_lit) {
                std::swap(clause->lits[0], clause->lits[1]);
            }
            
            int first_lit = clause->lits[0];
            int val_first = lit_val(first_lit);
            
            if (val_first == 1) {
                ws[j++] = Watcher{clause, first_lit};
                continue;
            }
            
            bool found_new = false;
            for (size_t k = 2; k < clause->lits.size(); ++k) {
                int alt_lit = clause->lits[k];
                if (lit_val(alt_lit) != -1) {
                    std::swap(clause->lits[1], clause->lits[k]);
                    watches[lit_idx(alt_lit)].push_back(Watcher{clause, first_lit});
                    found_new = true;
                    break;
                }
            }
            
            if (found_new) continue;
            
            ws[j++] = Watcher{clause, first_lit};
            if (val_first == -1) {
                conflict = clause;
                for (size_t k = i + 1; k < ws.size(); ++k) {
                    if (!ws[k].clause->deleted) ws[j++] = ws[k];
                }
                break;
            } else {
                assign_lit(first_lit, clause);
            }
        }
        
        ws.resize(j);
        if (conflict) return conflict;

        // 2. Tier-1 On-the-fly XOR propagation для назначенного литерала p
        if (xor_engine.size() > 0) {
            std::vector<std::pair<int, CDCLClause*>> xor_units;
            CDCLClause* xor_conf = xor_engine.propagate_var(std::abs(p), assigns, xor_units);
            if (xor_conf != nullptr) {
                xor_conflicts++;
                return xor_conf;
            }
            for (const auto& u : xor_units) {
                int u_lit = u.first;
                int var = std::abs(u_lit);
                if (assigns[var] == 0) {
                    assign_lit(u_lit, u.second);
                    xor_propagations++;
                } else if (lit_val(u_lit) == -1) {
                    xor_conflicts++;
                    return u.second;
                }
            }
        }

        // 3. Tier-1 On-the-fly Adder & Carry propagation отключена для предотвращения незвуковых reason-клозов
        // AdderEngine используется для структурного ранжирования переменных (get_bit_level)
    }
    return nullptr;
}

std::pair<std::vector<int>, int> CDCLSolver::analyze_conflict(CDCLClause* conflict_clause, int& out_lbd) {
    std::vector<int> learned;
    learned.push_back(0); // placeholder for UIP
    
    // VSIDS: затухание активностей раз в конфликт ВСЕГДА
    var_inc *= 1.0 / var_decay;
    
    int path_c = 0;
    int curr_lvl = current_level();
    int idx = static_cast<int>(trail.size()) - 1;
    int uip = 0;
    
    CDCLClause* confl = conflict_clause;
    
    while (true) {
        for (size_t i = (confl == conflict_clause) ? 0 : 1; i < confl->lits.size(); ++i) {
            int q = confl->lits[i];
            int var = std::abs(q);
            if (!seen[var] && decision_level[var] > 0) {
                seen[var] = true;
                // VSIDS: бамп всем переменным резолюции ВСЕГДА
                activity[var] += var_inc;
                if (activity[var] > 1e100) {
                    for (auto& a : activity) a *= 1e-100;
                    var_inc *= 1e-100;
                }
                if (decision_level[var] >= curr_lvl) {
                    path_c++;
                } else {
                    learned.push_back(q);
                }
            }
        }
        
        while (idx >= 0 && !seen[std::abs(trail[idx])]) {
            idx--;
        }
        if (idx < 0) {
            for (auto& s : seen) s = false;
            return {{}, 0};
        }
        
        int p = trail[idx];
        seen[std::abs(p)] = false;
        idx--;
        path_c--;
        
        if (path_c == 0) {
            uip = -p;
            break;
        }
        
        confl = reason[std::abs(p)];
        if (!confl) {
            for (auto& s : seen) s = false;
            return {{}, 0};
        }
    }
    
    learned[0] = uip;
    
    // Минимизация выученного клоза (self-subsumption) in-place:
    if (minimize_learned && learned.size() > 1) {
        size_t j = 1;
        for (size_t i = 1; i < learned.size(); ++i) {
            int lit = learned[i];
            int var = std::abs(lit);
            CDCLClause* r = reason[var];
            bool redundant = false;
            if (r != nullptr) {
                redundant = true;
                for (int q : r->lits) {
                    int v2 = std::abs(q);
                    if (v2 != var && decision_level[v2] > 0 && !seen[v2]) {
                        redundant = false;
                        break;
                    }
                }
            }
            if (!redundant) {
                learned[j++] = lit;
            } else {
                seen[var] = false; // немедленно снимаем метку с удаленного литерала
            }
        }
        learned.resize(j);
    }
    for (int lit : learned) {
        seen[std::abs(lit)] = false;
    }
    
    int backjump_lvl = 0;
    if (learned.size() > 1) {
        int max_i = 1;
        for (size_t i = 2; i < learned.size(); ++i) {
            if (decision_level[std::abs(learned[i])] > decision_level[std::abs(learned[max_i])]) {
                max_i = static_cast<int>(i);
            }
        }
        std::swap(learned[1], learned[max_i]);
        backjump_lvl = decision_level[std::abs(learned[1])];
    }
    
    // Fast O(K) LBD computation without sorting or heap allocation
    int lbd = 0;
    int epoch = conflicts + 1;
    for (int lit : learned) {
        int lvl = decision_level[std::abs(lit)];
        if (seen_level[lvl] != epoch) {
            seen_level[lvl] = epoch;
            lbd++;
        }
    }
    out_lbd = lbd;
    lbd_ema = 0.95 * lbd_ema + 0.05 * out_lbd;
    evolution.record_conflict(out_lbd);

    if (shared_pool != nullptr) {
        for (int lit : learned) {
            shared_pool->deposit_alarm(std::abs(lit), 1.0f);
        }
        if (out_lbd <= 2) {
            for (int lit : learned) {
                shared_pool->deposit_pheromone(lit, 2.0f / static_cast<float>(out_lbd));
            }
        }
    }
    
    return {learned, backjump_lvl};
}

void CDCLSolver::reduceDB() {
    std::vector<CDCLClause*> candidates;
    for (auto c : learned_clauses) {
        if (c->deleted) continue;
        
        // Клоз "заперт", если является reason для одного из наблюдаемых литералов
        bool locked = false;
        if (c->lits.size() > 0 && reason[std::abs(c->lits[0])] == c) {
            locked = true;
        } else if (c->lits.size() > 1 && reason[std::abs(c->lits[1])] == c) {
            locked = true;
        }
        
        if (!locked && c->lbd > 2 && c->lits.size() > 2) {
            // Агрессивное удаление мусора
            if (c->lbd > 5) {
                c->deleted = true;
            } else {
                candidates.push_back(c);
            }
        }
    }
    
    std::sort(candidates.begin(), candidates.end(), [](CDCLClause* a, CDCLClause* b) {
        if (a->lbd != b->lbd) return a->lbd > b->lbd; // delete max LBD first
        return a->lits.size() > b->lits.size();
    });
    
    int to_delete = static_cast<int>(candidates.size()) / 2;
    for (int i = 0; i < to_delete; ++i) {
        candidates[i]->deleted = true;
    }
    
    // In-place сжатие списков наблюдателей (без полной перестройки и реаллокаций)
    for (auto& ws : watches) {
        size_t j = 0;
        for (size_t i = 0; i < ws.size(); ++i) {
            if (!ws[i].clause->deleted) {
                ws[j++] = ws[i];
            }
        }
        ws.resize(j);
    }
    
    // Уплотнение clauses и централизованное освобождение памяти
    std::vector<CDCLClause*> kept_clauses;
    kept_clauses.reserve(clauses.size());
    for (auto c : clauses) {
        if (c->deleted) {
            free_clause(c);
        } else {
            kept_clauses.push_back(c);
        }
    }
    clauses.swap(kept_clauses);
    
    // Уплотняем original_clauses (только живые указатели, delete уже сделан выше)
    original_clauses.erase(
        std::remove_if(original_clauses.begin(), original_clauses.end(),
                       [](CDCLClause* c) { return c->deleted; }),
        original_clauses.end());

    // Уплотняем learned_clauses (только живые указатели, delete уже сделан выше)
    learned_clauses.erase(
        std::remove_if(learned_clauses.begin(), learned_clauses.end(),
                       [](CDCLClause* c) { return c->deleted; }),
        learned_clauses.end());
}

std::pair<int, int> CDCLSolver::validate_branch_with_shared_cubes(std::pair<int, int> branch) {
    if (!shared_pool || branch.first == 0 || current_level() >= max_cube_depth) {
        return branch;
    }
    
    std::vector<int> prefix;
    prefix.reserve(trail_lim.size() + 1);
    for (int lim : trail_lim) {
        if (lim < static_cast<int>(trail.size())) {
            prefix.push_back(trail[lim]);
        }
    }
    
    int var = branch.first;
    int lit = (branch.second == 1 ? var : -var);
    prefix.push_back(lit);
    
    if (shared_pool->try_claim_cube(thread_id, prefix)) {
        return branch;
    }
    
    // Куб занят другим потоком: инвертируем полярность для исследования альтернативной ветки
    prefix.back() = -lit;
    if (shared_pool->try_claim_cube(thread_id, prefix)) {
        shared_claims_skipped++;
        return {var, (branch.second == 1 ? 0 : 1)};
    }
    
    shared_claims_skipped++;
    return branch;
}

std::pair<int, int> CDCLSolver::pick_branch_lit() {
    // Адаптивный гибридный режим:
    // Если LBD_EMA высок (> 4.5), на формуле сильная диффузия/XOR-турбулентность ->
    // переключаемся преимущественно на VSIDS с физическим фазированием (85% VSIDS).
    // Если LBD_EMA низок (<= 4.5), формула структурирована (умножитель) ->
    // используем преимущественно физическую симуляцию (85% Физики).
    bool use_vsids = vsids_mode;
    if (!use_vsids) {
        if (lbd_ema > 4.5) {
            use_vsids = ((conflicts % 20) < 17); // 85% VSIDS в зашумленных задачах
        } else {
            use_vsids = ((conflicts % 20) >= 17); // 15% VSIDS в структурированных задачах
        }
    }
    
    if (use_vsids) {
        int active_cluster = num_clusters;
        if (!var_cluster.empty()) {
            for (int v = 1; v <= num_vars; ++v) {
                if (assigns[v] == 0 && var_alias[v] == 0 && !is_eliminated[v]) {
                    int c = var_cluster[v];
                    if (c < active_cluster) {
                        active_cluster = c;
                        if (active_cluster == 0) break;
                    }
                }
            }
        }

        int best_var = 0;
        double best_act = -1.0;
        for (int v = 1; v <= num_vars; ++v) {
            if (assigns[v] == 0 && var_alias[v] == 0 && !is_eliminated[v]) {
                double act = activity[v];
                if (!var_cluster.empty() && var_cluster[v] == active_cluster) {
                    act *= 1.10; // 10% tie-breaker активного 4D-кластера
                }
                if (adder_engine.fa_count() > 0 || adder_engine.ha_count() > 0) {
                    // Структурный LSB->MSB приоритет вдоль цепочек переносов
                    int bl = adder_engine.get_bit_level(v);
                    act *= (1.0 + 0.05 * std::max(0, 10 - bl));
                }
                if (act > best_act) {
                    best_act = act;
                    best_var = v;
                }
            }
        }
        if (best_var > 0) {
            // Физическое фазирование (Physics Phase Guidance):
            // если непрерывная физическая позиция частицы имеет выраженный наклон (>0.55 или <0.45),
            // используем физическую фазу вместо устаревшей сохраненной
            int phase = saved_phases[best_var];
            double pos = saved_positions[best_var];
            if (dim == 1) {
                if (pos > 0.55) phase = 1;
                else if (pos < 0.45) phase = 0;
            } else {
                if (pos > 0.05) phase = 1;
                else if (pos < -0.05) phase = 0;
            }

            // Муравьиный феромонный ориентир (ACO Swarm Bias)
            if (shared_pool != nullptr) {
                auto pheromone = shared_pool->query_pheromone(best_var);
                if (pheromone.first != 0 && pheromone.second > 0.35f) {
                    phase = (pheromone.first == 1) ? 1 : 0;
                }
            }

            return validate_branch_with_shared_cubes({best_var, phase});
        }
        for (int v = 1; v <= num_vars; ++v) {
            if (assigns[v] == 0 && var_alias[v] == 0 && !is_eliminated[v]) return validate_branch_with_shared_cubes({v, saved_phases[v]});
        }
        return {0, 0};
    }
    
    // 1. Кэш ранжирования: амортизация одной гонки на несколько решений
    if (race_uses < race_every) {
        while (race_cache_pos < race_cache.size()) {
            auto cand = race_cache[race_cache_pos++];
            if (assigns[cand.first] == 0 && var_alias[cand.first] == 0 && !is_eliminated[cand.first]) {
                race_uses++;
                cache_hits++;
                return cand;
            }
        }
    }
    
    auto t0 = std::chrono::high_resolution_clock::now();
    
    sim.clear_race();
    
    int unres_buf[128];
    int active_clauses_count = 0;

    for (auto c : original_clauses) {
        if (c->deleted) continue;
        const int* clits = c->lits.data();
        int clen = static_cast<int>(c->lits.size());
        if (clen >= 2) {
            if (lit_val(clits[0]) == 1 || lit_val(clits[1]) == 1) continue;
        }
        
        bool sat = false;
        int unres_len = 0;
        for (int i = 0; i < clen; ++i) {
            int lit = clits[i];
            int v = lit_val(lit);
            if (v == 1) {
                sat = true;
                break;
            } else if (v == 0) {
                if (unres_len < 128) {
                    unres_buf[unres_len++] = lit;
                }
            }
        }
        if (!sat && unres_len > 0) {
            sim.add_clause_fast(unres_buf, unres_len);
            active_clauses_count++;
        }
    }
    
    // Выученные клозы: только "клей" с низким LBD (ограничение входа физики).
    int added_learned = 0;
    for (auto c : learned_clauses) {
        if (c->deleted || c->lbd > physics_max_lbd) continue;
        if (added_learned > 5000 && c->lbd > 2) continue; // жесткий лимит
        
        const int* clits = c->lits.data();
        int clen = static_cast<int>(c->lits.size());
        if (clen >= 2) {
            if (lit_val(clits[0]) == 1 || lit_val(clits[1]) == 1) continue;
        }
        
        bool sat = false;
        int unres_len = 0;
        for (int i = 0; i < clen; ++i) {
            int lit = clits[i];
            int v = lit_val(lit);
            if (v == 1) {
                sat = true;
                break;
            } else if (v == 0) {
                if (unres_len < 128) {
                    unres_buf[unres_len++] = lit;
                }
            }
        }
        if (!sat && unres_len > 0) {
            sim.add_clause_fast(unres_buf, unres_len);
            active_clauses_count++;
            added_learned++;
        }
    }
    
    if (active_clauses_count > 0) {
        if (warm_steps > 0) {
            // Тёплая физика: состояние живёт в симуляторе, write-back не нужен
            race_cache = sim.pick_ranking_warm(warm_steps, 0.75);
        } else {
            race_cache = sim.pick_ranking(sim_steps, saved_positions);
            
            // Write-back позиций (позиционная память, как в Python-версии)
            const auto& final_pos = sim.get_positions();
            for (int v = 1; v <= num_vars; ++v) {
                if (assigns[v] == 0) saved_positions[v] = final_pos[v].x;
            }
        }
        race_cache_pos = 0;
        race_uses = 0;
        
        while (race_cache_pos < race_cache.size()) {
            auto cand = race_cache[race_cache_pos++];
            if (assigns[cand.first] == 0 && var_alias[cand.first] == 0 && !is_eliminated[cand.first]) {
                race_uses++;
                physics_time += std::chrono::duration<double>(std::chrono::high_resolution_clock::now() - t0).count();
                return validate_branch_with_shared_cubes(cand);
            }
        }
    }
    
    physics_time += std::chrono::duration<double>(std::chrono::high_resolution_clock::now() - t0).count();
    
    // Фолбэк: первая неназначенная переменная с сохранённой фазой
    for (int v = 1; v <= num_vars; ++v) {
        if (assigns[v] == 0 && var_alias[v] == 0 && !is_eliminated[v]) return validate_branch_with_shared_cubes({v, saved_phases[v]});
    }
    return {0, 0};
}

// Luby-фактор: luby_factor(0)=1, (1)=1, (2)=2, (3)=1, (4)=1, (5)=2, (6)=4, ...
static double luby_factor(int x) {
    int size, seq;
    for (size = 1, seq = 0; size < x + 1; seq++, size = 2 * size + 1) {}
    while (size - 1 != x) {
        size = (size - 1) >> 1;
        seq--;
        x = x % size;
    }
    return std::pow(2.0, seq);
}

SolveResult CDCLSolver::solve() {
    if (!ok) return SolveResult::UNSAT;
    
    auto conflict = propagate();
    if (conflict) return SolveResult::UNSAT;
    
    // Tier-1: Binary Implication Graph (BIG) + Tarjan SCC
    if (!run_big_inprocessing()) {
        return SolveResult::UNSAT;
    }

    // Фаза 2: Гауссов инпроцессинг над F_2 перед началом поиска
    if (!run_gauss_inprocessing()) {
        return SolveResult::UNSAT;
    }

    // Tier-1: Инициализация On-the-fly XOR Engine в трейле
    xor_engine.init(num_vars);
    xor_engine.extract_from_clauses(clauses);

    // BVE Preprocessing: элиминация промежуточных переменных строго ПОСЛЕ Гаусса и с защитой XOR
    if (!run_bve_preprocessing()) {
        return SolveResult::UNSAT;
    }

    // Пересобираем XOR-уравнения с учетом новых резольвент BVE
    xor_engine.extract_from_clauses(clauses);

    // Intel-style Carry & Adder Engine: распознавание Full/Half-Adders
    adder_engine.init(num_vars);
    adder_engine.extract_adders(clauses, xor_engine);
    full_adders_found = adder_engine.fa_count();
    half_adders_found = adder_engine.ha_count();

    // Фаза 3: Спектральная 4D-кластеризация графа формулы
    initialize_spectral_clusters();

    while (true) {
        // Фаза 4: Сигнал немедленного выхода от победившего потока
        if (shared_pool && shared_pool->terminate_all.load(std::memory_order_relaxed)) {
            return SolveResult::UNKNOWN;
        }
        
        // Редукция базы в точке решения (propagation догнан — GC watch-листов безопасен)
        if (conflicts >= next_reduce_conflict) {
            reduceDB();
            next_reduce_conflict += reduce_increment;
            reduce_increment += 100;
        }
        
        if (conflicts_since_restart >= restart_limit) {
            if (current_level() > 0) {
                backjump(0);
            }
            restarts++;
            conflicts_since_restart = 0;
            if (luby_restarts) {
                restart_limit = static_cast<int>(100 * luby_factor(restarts));
            } else {
                restart_limit = static_cast<int>(restart_limit * 1.5);
            }
            race_cache.clear(); // состояние изменилось радикально
            
            // Очистка временных XOR reason-клозов на уровне 0
            xor_engine.clear_reason_pool();

            // Эволюционный шаг самоадаптации параметров
            if (evolution.is_enabled()) {
                if (evolution.evolve_generation(conflicts)) {
                    const auto& g = evolution.get_genome();
                    var_decay = g.var_decay;
                    warm_steps = g.warm_steps;
                    physics_max_lbd = g.physics_max_lbd;
                    sim.set_evolution_params(g.spring_k, g.damping_c, g.deep_tunnel_dist, g.min_energy_gain);
                }
            }
            
            // Физическое квантовое туннелирование: возврат в контрольную точку + ортогональный импульс
            if (sim.has_checkpoint() && !last_conflict_lits.empty()) {
                sim.apply_conflict_tunneling(last_conflict_lits, 0.45f);
            }
            
            // Сброс отвергнутых кандидатов раз в 12 рестартов (защита от потери полноты)
            if (shared_pool && (restarts % 12 == 0) && thread_id == 0) {
                shared_pool->reset_rejected();
            }

            // Испарение феромонов раз в 8 рестартов (забывание ложных следов)
            if (shared_pool && (restarts % 8 == 0) && thread_id == 0) {
                shared_pool->evaporate_pheromones();
            }

            // Кооперативная синхронизация (PULL & APPLY строго на уровне 0!)
            if (shared_pool) {
                // 1. Синхронизация аксиом уровня 0 (Unit Bus)
                std::vector<int> new_units;
                shared_pool->sync_units(shared_unit_read_idx, new_units);
                for (int u : new_units) {
                    int val = lit_val(u);
                    if (val == -1) {
                        return SolveResult::UNSAT; // Доказанное противоречие на уровне 0
                    } else if (val == 0) {
                        assign_lit(u, nullptr);
                        shared_units_imported++;
                    }
                }

                // 2. Импорт высококачественных клозов (LBD <= 2) от других потоков
                std::vector<SharedClause> imported;
                shared_pool->import_clauses(thread_id, shared_read_idx, imported);
                for (const auto& sc : imported) {
                    if (sc.lits.empty()) continue;
                    if (sc.lits.size() == 1) {
                        int lit = sc.lits[0];
                        int val = lit_val(lit);
                        if (val == -1) {
                            return SolveResult::UNSAT; // Доказанное противоречие на уровне 0
                        } else if (val == 0) {
                            assign_lit(lit, nullptr);
                        }
                    } else if (sc.lits.size() >= 2) {
                        CDCLClause* cl = alloc_clause(sc.lits, true, sc.lbd);
                        clauses.push_back(cl);
                        learned_clauses.push_back(cl);
                        attach_clause(cl);
                    }
                }

                // 3. Синхронизация доказанных эквивалентностей (Equivalence Registry)
                std::vector<VerifiedEquiv> new_equivs;
                shared_pool->sync_equivalences(shared_equiv_read_idx, new_equivs);
                bool applied_any = false;

                for (const auto& eq : new_equivs) {
                    int lo = std::min(eq.v1, eq.v2), hi = std::max(eq.v1, eq.v2);
                    long long key = static_cast<long long>(lo) * (num_vars + 1) + hi;
                    plan_d_done.insert(key);

                    // Трансляция через локальные корни Union-Find
                    int r1 = find_root_lit(eq.v1);
                    int r2 = find_root_lit(eq.same ? eq.v2 : -eq.v2);
                    if (std::abs(r1) != std::abs(r2)) {
                        int var_to_alias = std::abs(r2);
                        int target_lit = (r2 > 0) ? r1 : -r1;
                        var_alias[var_to_alias] = target_lit;
                        substituted_vars++;
                        shared_equivs_imported++;
                        applied_any = true;
                    }
                }

                // 4. Дельта-гейтинг: apply_substitutions() ТОЛЬКО если дельта не пуста!
                if (applied_any) {
                    if (!apply_substitutions()) {
                        return SolveResult::UNSAT;
                    }
                }

                auto conf = propagate();
                if (conf) return SolveResult::UNSAT;
            }
            
            // План D: Спектральная развертка (генератор кандидатов) + Фаза 1:
            // UP-верификация. Физика только ПРЕДЛАГАЕТ пары; инжект разрешён
            // лишь после доказательства эквивалентности юнит-пропагацией —
            // влитые клозы имплицированы формулой, звукость и полнота
            // сохраняются (ложный UNSAT невозможен).
            if (restarts % 3 == 0 && (plan_d_candidates == 0 || plan_d_verified > 0 || restarts <= 6)) {
                sim.clear_race();
                int unres_buf[128];
                for (auto c : original_clauses) {
                    if (c->deleted) continue;
                    const int* clits = c->lits.data();
                    int clen = static_cast<int>(c->lits.size());
                    bool sat = false;
                    int unres_len = 0;
                    for (int i = 0; i < clen; ++i) {
                        int lit = clits[i];
                        int v = lit_val(lit);
                        if (v == 1) { sat = true; break; }
                        else if (v == 0) {
                            if (unres_len < 128) unres_buf[unres_len++] = lit;
                        }
                    }
                    if (!sat && unres_len > 0) sim.add_clause_fast(unres_buf, unres_len);
                }
                auto equivs = sim.extract_correlations();
                bool found_new_equiv = false;
                for (auto& pair : equivs) {
                    int v1 = pair.first;
                    int v2 = std::abs(pair.second);
                    bool same = (pair.second > 0);

                    if (assigns[v1] != 0 || assigns[v2] != 0) continue;
                    if (var_alias[v1] != 0 || var_alias[v2] != 0) continue;

                    // dedup: уже влитую эквивалентность не пересчитываем
                    int lo = std::min(v1, v2), hi = std::max(v1, v2);
                    long long key = static_cast<long long>(lo) * (num_vars + 1) + hi;
                    if (plan_d_done.count(key)) continue;

                    // Кооперативный захват кандидата: не проверять параллельно с другим потоком
                    if (shared_pool) {
                        if (!shared_pool->try_claim_candidate(v1, v2, thread_id, num_vars)) {
                            shared_claims_skipped++;
                            continue;
                        }
                    }

                    plan_d_candidates++;

                    // Фаза 1: инжект только ДОКАЗАННЫХ эквивалентностей
                    if (!verify_equivalence(v1, v2, same)) {
                        if (shared_pool) {
                            shared_pool->publish_rejection(v1, v2, num_vars);
                        }
                        continue;
                    }
                    plan_d_verified++;
                    plan_d_done.insert(key);
                    if (shared_pool) {
                        shared_pool->publish_equivalence(v1, v2, same, num_vars);
                    }

                    // Перманентная подстановка Union-Find: исключаем v2
                    int r1 = find_root_lit(v1);
                    int r2 = find_root_lit(same ? v2 : -v2);
                    if (std::abs(r1) != std::abs(r2)) {
                        int var_to_alias = std::abs(r2);
                        int target_lit = (r2 > 0) ? r1 : -r1;
                        var_alias[var_to_alias] = target_lit;
                        substituted_vars++;
                        found_new_equiv = true;
                    }
                }

                if (found_new_equiv) {
                    if (!apply_substitutions()) {
                        return SolveResult::UNSAT;
                    }
                }
            }
            
            // Периодический прогон BIG + Гаусса на рестартах
            if (restarts % 6 == 0) {
                if (!run_big_inprocessing()) {
                    return SolveResult::UNSAT;
                }
                if (!run_gauss_inprocessing()) {
                    return SolveResult::UNSAT;
                }
                xor_engine.extract_from_clauses(clauses);
            }

            // Фаза 5: Vivification выученных клозов на каждом 4-м рестарте
            if (restarts % 4 == 0) {
                vivify_clauses();
                if (!ok) return SolveResult::UNSAT;
            }

            // Target Phasing: каждые 8 рестартов обновляем фазы из глубочайшего бесконфликтного трейла
            if (restarts % 8 == 0) {
                for (int v = 1; v <= num_vars; ++v) {
                    saved_phases[v] = target_phases[v];
                }
            }
            
            continue;
        }
        
        decisions_count++;
        evolution.record_decision();
        auto p = pick_branch_lit();
        int var = p.first;
        int pol = p.second;
        if (var == 0) {
            // Восстановление полной модели: сначала алиасы, затем BVE по теореме SatElite
            reconstruct_model();

            // Дозаполнение неназначенных переменных сохраненными фазами
            for (int v = 1; v <= num_vars; ++v) {
                if (assigns[v] == 0) {
                    assigns[v] = (saved_phases[v] == 1 ? 1 : -1);
                }
            }
            return SolveResult::SAT;
        }
        
        new_decision_level();
        int lit = (pol == 1) ? var : -var;
        assign_lit(lit, nullptr);

        if (static_cast<int>(trail.size()) > best_trail_size) {
            for (size_t i = best_trail_size; i < trail.size(); ++i) {
                int tlit = trail[i];
                target_phases[std::abs(tlit)] = (tlit > 0 ? 1 : 0);
            }
            best_trail_size = static_cast<int>(trail.size());
            sim.save_checkpoint();
        }
        
        while (true) {
            if (shared_pool && shared_pool->terminate_all.load(std::memory_order_relaxed)) {
                return SolveResult::UNKNOWN;
            }
            conflict = propagate();
            if (!conflict) break;
            
            conflicts++;
            if (conflicts >= max_conflicts) return SolveResult::UNKNOWN;
            if (current_level() == 0) return SolveResult::UNSAT;
            
            int lbd = 0;
            auto bj = analyze_conflict(conflict, lbd);
            std::vector<int> learned_lits = bj.first;
            int bj_lvl = bj.second;

            if (learned_lits.empty() || learned_lits[0] == 0) {
                return (current_level() == 0) ? SolveResult::UNSAT : SolveResult::UNKNOWN;
            }
            
            last_conflict_lits = learned_lits;
            
            CDCLClause* lcl = alloc_clause(learned_lits, true, lbd);
            
            clauses.push_back(lcl);
            learned_clauses.push_back(lcl);
            
            conflicts_since_restart++;
            
            backjump(bj_lvl);
            
            if (learned_lits.size() >= 2) {
                attach_clause(lcl);
                assign_lit(learned_lits[0], lcl);
                if (shared_pool && lbd <= 2) {
                    shared_pool->export_clause(thread_id, learned_lits, lbd);
                }
            } else if (learned_lits.size() == 1) {
                assign_lit(learned_lits[0], lcl);
                if (shared_pool) {
                    shared_pool->export_clause(thread_id, learned_lits, 1);
                    shared_pool->publish_unit(learned_lits[0]);
                }
            }
        }
    }
}
