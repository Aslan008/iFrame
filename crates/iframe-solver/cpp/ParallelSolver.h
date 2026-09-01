#pragma once
#include <vector>
#include <string>
#include <thread>
#include <mutex>
#include <atomic>
#include <memory>
#include <unordered_map>
#include <algorithm>
#include "Solver.h"

struct SharedClause {
    std::vector<int> lits;
    int lbd;
    int source_thread = -1;
};

struct VerifiedEquiv {
    int v1;
    int v2;
    bool same;
};

struct CubeKey {
    int len;
    int lits[16];

    bool operator==(const CubeKey& o) const {
        if (len != o.len) return false;
        for (int i = 0; i < len; ++i) {
            if (lits[i] != o.lits[i]) return false;
        }
        return true;
    }
};

struct CubeKeyHash {
    size_t operator()(const CubeKey& k) const {
        size_t h = 0xcbf29ce484222325ULL;
        for (int i = 0; i < k.len; ++i) {
            h ^= static_cast<size_t>(k.lits[i] * 2654435761ULL);
            h *= 0x100000001b3ULL;
        }
        return h;
    }
};

// Муравьиное феромонное поле (Ant Colony Optimization / ACO)
struct PheromoneSignal {
    float pos_pheromone;   // феромон привлекательности фазы +1 (True)
    float neg_pheromone;   // феромон привлекательности фазы -1 (False)
    float alarm_pheromone; // феромон тревоги/опасности (конфликтный тупик)
};

class SharedPheromoneMatrix {
public:
    SharedPheromoneMatrix(int num_vars = 0, float decay_rate = 0.05f)
        : num_vars(num_vars), decay_rate(decay_rate) {
        if (num_vars > 0) init(num_vars);
    }

    void init(int nvars) {
        num_vars = nvars;
        signals.assign(num_vars + 1, PheromoneSignal{1.0f, 1.0f, 0.0f});
    }

    void deposit_trail(int lit, float strength = 1.0f) {
        int v = std::abs(lit);
        if (v < 1 || v > num_vars) return;

        std::lock_guard<std::mutex> lock(phero_mutex);
        if (lit > 0) signals[v].pos_pheromone += strength;
        else signals[v].neg_pheromone += strength;
    }

    void deposit_alarm(int var, float alarm_strength = 2.0f) {
        if (var < 1 || var > num_vars) return;
        std::lock_guard<std::mutex> lock(phero_mutex);
        signals[var].alarm_pheromone += alarm_strength;
    }

    void evaporate() {
        std::lock_guard<std::mutex> lock(phero_mutex);
        for (int v = 1; v <= num_vars; ++v) {
            signals[v].pos_pheromone = std::max(0.1f, signals[v].pos_pheromone * (1.0f - decay_rate));
            signals[v].neg_pheromone = std::max(0.1f, signals[v].neg_pheromone * (1.0f - decay_rate));
            signals[v].alarm_pheromone = std::max(0.0f, signals[v].alarm_pheromone * (1.0f - decay_rate * 2.0f));
        }
    }

    std::pair<int, float> query_pheromone(int var) const {
        if (var < 1 || var > num_vars) return {0, 0.0f};
        std::lock_guard<std::mutex> lock(phero_mutex);
        float pos = signals[var].pos_pheromone;
        float neg = signals[var].neg_pheromone;
        float alarm = signals[var].alarm_pheromone;

        if (pos > neg * 1.25f) return {1, (pos - neg) / (pos + neg + alarm + 0.1f)};
        if (neg > pos * 1.25f) return {-1, (neg - pos) / (pos + neg + alarm + 0.1f)};
        return {0, 0.0f};
    }

private:
    mutable std::mutex phero_mutex;
    int num_vars = 0;
    float decay_rate = 0.05f;
    std::vector<PheromoneSignal> signals;
};

class SharedClausePool {
public:
    SharedClausePool(size_t max_pool_size = 50000);

    // Экспорт качественного выученного клоза (LBD <= 2) в общий пул
    void export_clause(int thread_id, const std::vector<int>& lits, int lbd);

    // Импорт новых выученных клозов для конкретного потока
    void import_clauses(int thread_id, size_t& last_read_idx, std::vector<SharedClause>& out_clauses);

    // 1. Кооперативный реестр эквивалентностей Плана D (Shared Equiv Registry)
    bool try_claim_candidate(int v1, int v2, int thread_id, int num_vars);
    void publish_equivalence(int v1, int v2, bool same, int num_vars);
    void publish_rejection(int v1, int v2, int num_vars);
    void sync_equivalences(size_t& last_read_idx, std::vector<VerifiedEquiv>& out_equivs);
    void reset_rejected();

    // 2. Кооперативная шина литералов уровня 0 (Shared Unit Bus)
    void publish_unit(int lit);
    void sync_units(size_t& last_read_idx, std::vector<int>& out_units);

    // 3. Динамический реестр бронирования кубов решений (Dynamic Search Space Reservation)
    bool try_claim_cube(int thread_id, const std::vector<int>& prefix);
    void release_cube(int thread_id);
    void publish_refuted_cube(const std::vector<int>& prefix);

    // 4. Муравьиное феромонное поле роевого интеллекта (Ant Colony Pheromones)
    void init_pheromones(int num_vars) { pheromones.init(num_vars); }
    void deposit_pheromone(int lit, float strength = 1.0f) { pheromones.deposit_trail(lit, strength); }
    void deposit_alarm(int var, float alarm_strength = 2.0f) { pheromones.deposit_alarm(var, alarm_strength); }
    void evaporate_pheromones() { pheromones.evaporate(); }
    std::pair<int, float> query_pheromone(int var) const { return pheromones.query_pheromone(var); }

    std::atomic<bool> terminate_all{false};
    std::atomic<SolveResult> winner_result{SolveResult::UNKNOWN};
    std::vector<int> winner_assigns;
    int winning_thread{-1};
    std::mutex finish_mutex;

private:
    std::mutex pool_mutex;
    std::vector<SharedClause> pool;
    size_t max_pool_size;
    uint64_t total_exported{0};

    // Реестр эквивалентностей Плана D
    std::mutex equiv_mutex;
    std::unordered_map<long long, int8_t> pair_status;
    std::vector<VerifiedEquiv> verified_equivs;

    // Шина юнитов уровня 0 с O(1) дедупликацией
    std::mutex unit_mutex;
    std::vector<int> unit_bus;
    std::vector<uint8_t> seen_units;

    // Реестр забронированных кубов (Guiding Paths)
    std::mutex cube_mutex;
    std::unordered_map<CubeKey, int, CubeKeyHash> active_cubes;
    std::unordered_map<int, CubeKey> thread_current_cube;

    // Общее феромонное поле
    SharedPheromoneMatrix pheromones;
};

class ParallelPortfolioSolver {
public:
    ParallelPortfolioSolver(int num_vars,
                            const std::vector<std::vector<int>>& initial_clauses,
                            int num_threads = 4,
                            int max_conflicts = 200000,
                            int sim_steps = 18,
                            int physics_max_lbd = 3,
                            int race_every = 4);

    SolveResult solve();

    std::vector<int> assigns;
    long long total_conflicts = 0;
    long long total_decisions = 0;
    long long total_propagations = 0;
    long long total_shared_equivs = 0;
    long long total_shared_units = 0;
    long long total_shared_claims_skipped = 0;
    double wall_time = 0.0;
    int winning_thread = -1;

private:
    int num_vars;
    std::vector<std::vector<int>> initial_clauses;
    int num_threads;
    int max_conflicts;
    int sim_steps;
    int physics_max_lbd;
    int race_every;

    SharedClausePool shared_pool;
};
