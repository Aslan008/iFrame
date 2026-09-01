#pragma once
#include <vector>
#include <cstdint>
#include <atomic>

class SharedClausePool;

struct MultiplierStructure {
    bool valid{false};
    int n_bits{0};
    uint64_t target_N{0};
    std::vector<int> p_vars;
    std::vector<int> q_vars;
    std::vector<int> out_vars;
};

class TheoryOracle {
public:
    TheoryOracle() = default;
    ~TheoryOracle() = default;

    // Распознавание структуры схемы умножения из CNF
    static MultiplierStructure analyze_multiplier(int num_vars, const std::vector<std::vector<int>>& clauses);

    // Фоновый запуск математического оракула
    static bool run_oracle(int num_vars,
                           const std::vector<std::vector<int>>& clauses,
                           SharedClausePool& pool,
                           std::atomic<bool>& stop_flag);

    // Прямая симуляция схемы для построения 100% полной модели по найденным множителям
    static bool simulate_and_complete_model(int num_vars,
                                            const std::vector<std::vector<int>>& clauses,
                                            const std::vector<int>& p_vars,
                                            const std::vector<int>& q_vars,
                                            uint64_t P,
                                            uint64_t Q,
                                            std::vector<int>& out_assigns);
};
