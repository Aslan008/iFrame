#pragma once
#include <vector>
#include <cstdint>
#include <string>

struct MarchCubeStats {
    int generated_cubes = 0;
    int pruned_branches = 0;
    double time_s = 0.0;
    std::vector<int> pivot_vars;
};

// Генератор адаптивных кубов на базе двунаправленного Lookahead / UP-скоринга
class MarchCubingEngine {
public:
    MarchCubingEngine(int num_vars, const std::vector<std::vector<int>>& base_clauses);
    ~MarchCubingEngine();

    // Генерация сбалансированного множества кубов
    std::vector<std::vector<int>> generate_cubes(
        int max_cubes = 64,
        int max_depth = 8,
        const std::vector<int>& priority_vars = {}
    );

    MarchCubeStats get_stats() const { return stats; }

private:
    int num_vars;
    const std::vector<std::vector<int>>& clauses;
    MarchCubeStats stats;

    struct VarScore {
        int var;
        double score;
    };

    // Оценка переменной методом пробной Unit Propagation
    double evaluate_var_lookahead(
        int var,
        const std::vector<int8_t>& current_assigns,
        const std::vector<std::vector<int>>& adj_clauses
    );

    void build_cube_tree(
        std::vector<int>& current_cube,
        std::vector<int8_t>& current_assigns,
        int depth,
        int max_depth,
        int max_cubes,
        const std::vector<int>& candidate_vars,
        std::vector<std::vector<int>>& out_cubes
    );
};
