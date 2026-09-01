#pragma once
#include <vector>
#include <unordered_map>
#include <algorithm>
#include <cstdint>

struct CDCLClause;
class XOREngine;

struct HalfAdder {
    int a;      // Input 1 (var)
    int b;      // Input 2 (var)
    int sum;    // Sum output (a ^ b)
    int carry;  // Carry output (a & b)
    CDCLClause* reason_clause = nullptr;
};

struct FullAdder {
    int a;      // Input 1 (var)
    int b;      // Input 2 (var)
    int cin;    // Carry in (var)
    int sum;    // Sum output (a ^ b ^ cin)
    int cout;   // Carry out (maj(a, b, cin))
    CDCLClause* reason_clause = nullptr;
};

class AdderEngine {
public:
    explicit AdderEngine(int num_vars = 0);
    ~AdderEngine() = default;

    void init(int num_vars);

    // Автоматическое распознавание узлов Full-Adder и Half-Adder из CNF и XOR-движка
    int extract_adders(const std::vector<CDCLClause*>& clauses, const XOREngine& xor_engine);

    int fa_count() const { return static_cast<int>(full_adders.size()); }
    int ha_count() const { return static_cast<int>(half_adders.size()); }

    // Получить топологический приоритет переменной вдоль цепочки переносов (LSB -> MSB)
    int get_bit_level(int var) const {
        if (var >= 1 && var < static_cast<int>(bit_levels.size())) {
            return bit_levels[var];
        }
        return 0;
    }

    // On-the-fly арифметическая и Carry дедукция в цикле propagate()
    CDCLClause* propagate_var(
        int var,
        const std::vector<int>& assigns,
        std::vector<std::pair<int, CDCLClause*>>& out_units
    );

private:
    int num_vars;
    std::vector<FullAdder> full_adders;
    std::vector<HalfAdder> half_adders;

    std::vector<std::vector<int>> var_to_fa; // var -> список индексов full_adders
    std::vector<std::vector<int>> var_to_ha; // var -> список индексов half_adders
    std::vector<int> bit_levels;             // топологическая глубина в DAG переносов
};
