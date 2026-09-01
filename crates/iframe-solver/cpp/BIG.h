#pragma once
#include <vector>
#include <algorithm>
#include <cmath>

struct CDCLClause;

class BinaryImplicationGraph {
public:
    explicit BinaryImplicationGraph(int num_vars);

    // Извлечение ориентированных рёбер импликаций из бинарных клозов
    int extract_from_clauses(const std::vector<CDCLClause*>& clauses);

    // Итеративный алгоритм Тарьяна поиска компонент сильной связности (SCC)
    // Возвращает false, если обнаружено противоречие (UNSAT), иначе true
    bool find_sccs_and_substitute(std::vector<int>& var_alias,
                                  std::vector<int>& level0_units,
                                  int& equiv_count,
                                  int (*find_root_fn)(const std::vector<int>&, int));

    static inline int lit_to_idx(int lit) {
        return (lit > 0) ? (lit << 1) : (((-lit) << 1) | 1);
    }

    static inline int idx_to_lit(int idx) {
        return (idx % 2 == 0) ? (idx >> 1) : -(idx >> 1);
    }

private:
    int num_vars;
    int num_nodes;
    std::vector<std::vector<int>> adj;
};
