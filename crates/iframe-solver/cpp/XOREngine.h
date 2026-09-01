#pragma once
#include <vector>
#include <algorithm>
#include <cstdint>

struct CDCLClause;

struct XOREquation {
    std::vector<int> vars; // список переменных 1..num_vars (отсортирован)
    bool rhs;              // целевая четность (0 или 1)
};

class XOREngine {
public:
    explicit XOREngine(int num_vars = 0);
    ~XOREngine();

    void init(int num_vars);

    // Извлечение XOR-уравнений из базы клозов
    int extract_from_clauses(const std::vector<CDCLClause*>& clauses);

    // On-the-fly BCP-пропагация для переменной
    // Возвращает conflict clause при несовпадении четности (иначе nullptr)
    // Добавляет новые юнит-присваивания в to_assign
    CDCLClause* propagate_var(int assigned_var,
                              const std::vector<int>& assigns,
                              std::vector<std::pair<int, CDCLClause*>>& to_assign);

    // Очистка пула reason/conflict клозов на рестартах (при decision_level == 0)
    void clear_reason_pool();

    int size() const { return static_cast<int>(equations.size()); }
    const std::vector<XOREquation>& get_equations() const { return equations; }

private:
    int num_vars;
    std::vector<XOREquation> equations;
    std::vector<std::vector<int>> xor_watches; // var -> список индексов XOR-уравнений
    std::vector<CDCLClause*> reason_pool;      // динамические reason/conflict клозы
};
