#include "ParallelSolver.h"
#include "TheoryOracle.h"
#include <chrono>
#include <iostream>

SharedClausePool::SharedClausePool(size_t max_pool_size)
    : max_pool_size(max_pool_size) {
    pool.reserve(max_pool_size);
}

void SharedClausePool::export_clause(int thread_id, const std::vector<int>& lits, int lbd) {
    if (lits.empty() || lbd > 2) return;
    std::lock_guard<std::mutex> lock(pool_mutex);
    if (pool.size() < max_pool_size) {
        pool.push_back({lits, lbd, thread_id});
    } else {
        pool[total_exported % max_pool_size] = {lits, lbd, thread_id};
    }
    total_exported++;
}

void SharedClausePool::import_clauses(int thread_id, size_t& last_read_idx, std::vector<SharedClause>& out_clauses) {
    out_clauses.clear();
    std::lock_guard<std::mutex> lock(pool_mutex);
    if (total_exported > max_pool_size && last_read_idx < (total_exported - max_pool_size)) {
        last_read_idx = total_exported - max_pool_size;
    }
    while (last_read_idx < total_exported) {
        const auto& sc = pool[last_read_idx % max_pool_size];
        if (sc.source_thread != thread_id) {
            out_clauses.push_back(sc);
        }
        last_read_idx++;
    }
}

static inline long long make_pair_key(int v1, int v2, int num_vars) {
    int lo = std::min(v1, v2);
    int hi = std::max(v1, v2);
    return static_cast<long long>(lo) * (num_vars + 1) + hi;
}

bool SharedClausePool::try_claim_candidate(int v1, int v2, int /*thread_id*/, int num_vars) {
    long long key = make_pair_key(v1, v2, num_vars);
    std::lock_guard<std::mutex> lock(equiv_mutex);
    auto it = pair_status.find(key);
    if (it != pair_status.end()) {
        return false;
    }
    pair_status[key] = 1; // Mark as PROVING
    return true;
}

void SharedClausePool::publish_equivalence(int v1, int v2, bool same, int num_vars) {
    long long key = make_pair_key(v1, v2, num_vars);
    std::lock_guard<std::mutex> lock(equiv_mutex);
    pair_status[key] = same ? 2 : 3;
    verified_equivs.push_back({v1, v2, same});
}

void SharedClausePool::publish_rejection(int v1, int v2, int num_vars) {
    long long key = make_pair_key(v1, v2, num_vars);
    std::lock_guard<std::mutex> lock(equiv_mutex);
    pair_status[key] = -1; // REJECTED
}

void SharedClausePool::sync_equivalences(size_t& last_read_idx, std::vector<VerifiedEquiv>& out_equivs) {
    out_equivs.clear();
    std::lock_guard<std::mutex> lock(equiv_mutex);
    if (last_read_idx < verified_equivs.size()) {
        out_equivs.assign(verified_equivs.begin() + last_read_idx, verified_equivs.end());
        last_read_idx = verified_equivs.size();
    }
}

void SharedClausePool::reset_rejected() {
    std::lock_guard<std::mutex> lock(equiv_mutex);
    for (auto it = pair_status.begin(); it != pair_status.end(); ) {
        if (it->second == -1) {
            it = pair_status.erase(it);
        } else {
            ++it;
        }
    }
}

void SharedClausePool::publish_unit(int lit) {
    if (lit == 0) return;
    int lit_i = (lit > 0) ? (lit << 1) : (((-lit) << 1) | 1);
    std::lock_guard<std::mutex> lock(unit_mutex);
    if (static_cast<size_t>(lit_i) >= seen_units.size()) {
        seen_units.resize(lit_i + 1024, 0);
    }
    if (seen_units[lit_i]) return;
    seen_units[lit_i] = 1;
    unit_bus.push_back(lit);
}

void SharedClausePool::sync_units(size_t& last_read_idx, std::vector<int>& out_units) {
    out_units.clear();
    std::lock_guard<std::mutex> lock(unit_mutex);
    if (last_read_idx < unit_bus.size()) {
        out_units.assign(unit_bus.begin() + last_read_idx, unit_bus.end());
        last_read_idx = unit_bus.size();
    }
}

bool SharedClausePool::try_claim_cube(int thread_id, const std::vector<int>& prefix) {
    if (prefix.empty()) return true;
    CubeKey key;
    key.len = std::min(static_cast<int>(prefix.size()), 16);
    for (int i = 0; i < key.len; ++i) {
        key.lits[i] = prefix[i];
    }

    std::lock_guard<std::mutex> lock(cube_mutex);
    auto it = active_cubes.find(key);
    if (it != active_cubes.end()) {
        if (it->second == thread_id) return true; // уже забронирован этим же потоком
        return false; // занят другим параллельным потоком!
    }
    
    // Освобождаем старый куб этого потока, если был
    auto cur_it = thread_current_cube.find(thread_id);
    if (cur_it != thread_current_cube.end()) {
        active_cubes.erase(cur_it->second);
    }
    
    active_cubes[key] = thread_id;
    thread_current_cube[thread_id] = key;
    return true;
}

void SharedClausePool::release_cube(int thread_id) {
    std::lock_guard<std::mutex> lock(cube_mutex);
    auto cur_it = thread_current_cube.find(thread_id);
    if (cur_it != thread_current_cube.end()) {
        active_cubes.erase(cur_it->second);
        thread_current_cube.erase(cur_it);
    }
}

void SharedClausePool::publish_refuted_cube(const std::vector<int>& prefix) {
    if (prefix.empty()) return;
    // Клоз опровержения куба: ~(l1 & l2 & ... & lk) = (~l1 | ~l2 | ... | ~lk)
    std::vector<int> blocking_clause;
    blocking_clause.reserve(prefix.size());
    for (int lit : prefix) {
        blocking_clause.push_back(-lit);
    }
    export_clause(-1, blocking_clause, 1);
}

ParallelPortfolioSolver::ParallelPortfolioSolver(int num_vars,
                                               const std::vector<std::vector<int>>& initial_clauses,
                                               int num_threads,
                                               int max_conflicts,
                                               int sim_steps,
                                               int physics_max_lbd,
                                               int race_every)
    : num_vars(num_vars),
      initial_clauses(initial_clauses),
      num_threads(num_threads),
      max_conflicts(max_conflicts),
      sim_steps(sim_steps),
      physics_max_lbd(physics_max_lbd),
      race_every(race_every),
      shared_pool(50000) {
    if (this->num_threads <= 0) {
        this->num_threads = static_cast<int>(std::thread::hardware_concurrency());
        if (this->num_threads <= 0) this->num_threads = 4;
    }
}

SolveResult ParallelPortfolioSolver::solve() {
    auto t_start = std::chrono::high_resolution_clock::now();

    std::vector<std::thread> workers;
    workers.reserve(num_threads);

    std::vector<std::unique_ptr<CDCLSolver>> solvers;
    solvers.reserve(num_threads);

    std::vector<SolveResult> thread_results(num_threads, SolveResult::UNKNOWN);

    for (int tid = 0; tid < num_threads; ++tid) {
        int thread_dim = (tid % 2 == 1) ? 1 : 4;
        int thread_clusters = 4;
        bool thread_luby = (tid % 2 == 0);
        bool thread_vsids = (tid % 4 == 3);
        int thread_warm = (tid % 4 == 0) ? 4 : ((tid % 4 == 1) ? 2 : ((tid % 4 == 2) ? 6 : 0));
        double thread_decay = 0.95 - (tid % 4) * 0.02;

        if (tid == 1) thread_clusters = 8;
        else if (tid == 2) thread_clusters = 2;

        auto s = std::make_unique<CDCLSolver>(num_vars, initial_clauses, max_conflicts, sim_steps, physics_max_lbd, race_every, thread_dim);
        s->shared_pool = &shared_pool;
        s->thread_id = tid;
        s->luby_restarts = thread_luby;
        s->vsids_mode = thread_vsids;
        s->warm_steps = thread_warm;
        s->var_decay = thread_decay;
        s->num_clusters = thread_clusters;

        solvers.push_back(std::move(s));
    }

    // Запуск математического сопроцессора-оракула (Theory-Guided Arithmetic Oracle)
    std::thread oracle_thread([this]() {
        TheoryOracle::run_oracle(num_vars, initial_clauses, shared_pool, shared_pool.terminate_all);
    });

    for (int tid = 0; tid < num_threads; ++tid) {
        workers.emplace_back([this, tid, &solvers, &thread_results]() {
            auto res = solvers[tid]->solve();
            thread_results[tid] = res;

            if (res == SolveResult::SAT || res == SolveResult::UNSAT) {
                std::lock_guard<std::mutex> lock(shared_pool.finish_mutex);
                if (!shared_pool.terminate_all.load(std::memory_order_relaxed)) {
                    shared_pool.winner_result.store(res, std::memory_order_relaxed);
                    shared_pool.winner_assigns = solvers[tid]->assigns;
                    shared_pool.winning_thread = tid;
                    shared_pool.terminate_all.store(true, std::memory_order_relaxed);
                }
            }
        });
    }

    for (auto& w : workers) {
        if (w.joinable()) {
            w.join();
        }
    }

    // Сигнализируем оракулу о завершении и ждём его
    shared_pool.terminate_all.store(true, std::memory_order_relaxed);
    if (oracle_thread.joinable()) {
        oracle_thread.join();
    }

    auto t_end = std::chrono::high_resolution_clock::now();
    wall_time = std::chrono::duration<double>(t_end - t_start).count();

    for (int tid = 0; tid < num_threads; ++tid) {
        total_conflicts += solvers[tid]->conflicts;
        total_propagations += solvers[tid]->propagations;
        total_shared_equivs += solvers[tid]->shared_equivs_imported;
        total_shared_units += solvers[tid]->shared_units_imported;
        total_shared_claims_skipped += solvers[tid]->shared_claims_skipped;
    }

    winning_thread = shared_pool.winning_thread;
    if (winning_thread >= 0) {
        assigns = std::move(shared_pool.winner_assigns);
        return shared_pool.winner_result.load();
    }

    // Если ни один поток не решил задачу до лимита
    for (int tid = 0; tid < num_threads; ++tid) {
        if (thread_results[tid] == SolveResult::SAT || thread_results[tid] == SolveResult::UNSAT) {
            assigns = solvers[tid]->assigns;
            winning_thread = tid;
            return thread_results[tid];
        }
    }

    return SolveResult::UNKNOWN;
}
