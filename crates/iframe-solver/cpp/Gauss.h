#pragma once
#include <vector>
#include <cstdint>
#include <algorithm>
#include <iostream>

struct CDCLClause;

struct XorRow {
    std::vector<uint64_t> cols; // Битовый вектор переменных 1..N
    bool rhs;                   // Правая часть (0 или 1)

    XorRow() : rhs(false) {}
    XorRow(size_t num_words, bool rhs = false) : cols(num_words, 0), rhs(rhs) {}

    inline void set_bit(int var) {
        if (var <= 0) return;
        size_t idx = static_cast<size_t>(var - 1);
        cols[idx >> 6] ^= (1ULL << (idx & 63));
    }

    inline bool get_bit(int var) const {
        if (var <= 0) return false;
        size_t idx = static_cast<size_t>(var - 1);
        return (cols[idx >> 6] & (1ULL << (idx & 63))) != 0;
    }

    inline bool is_empty() const {
        for (uint64_t w : cols) {
            if (w != 0) return false;
        }
        return true;
    }

    inline int leading_var() const {
        for (size_t i = 0; i < cols.size(); ++i) {
            if (cols[i] != 0) {
                // Поиск младшего установленного бита
                uint64_t w = cols[i];
#if defined(_MSC_VER)
                unsigned long index;
                _BitScanForward64(&index, w);
                return static_cast<int>(i * 64 + index + 1);
#else
                return static_cast<int>(i * 64 + __builtin_ctzll(w) + 1);
#endif
            }
        }
        return 0;
    }

    inline int popcount() const {
        int count = 0;
        for (uint64_t w : cols) {
#if defined(_MSC_VER)
            count += static_cast<int>(__popcnt64(w));
#else
            count += __builtin_popcountll(w);
#endif
        }
        return count;
    }

    inline void xor_with(const XorRow& other) {
        for (size_t i = 0; i < cols.size(); ++i) {
            cols[i] ^= other.cols[i];
        }
        rhs ^= other.rhs;
    }
};

class GaussEngine {
public:
    GaussEngine(int num_vars);

    // Автодетекция XOR-цепочек из базы клозов CNF
    int extract_xors_from_clauses(const std::vector<CDCLClause*>& clauses);

    // Добавление одиночного XOR-уравнения (v1 ^ v2 ^ ... ^ vk = rhs)
    void add_xor_clause(const std::vector<int>& vars, bool rhs);

    // Выполнение метода Гаусса над F_2 (RREF):
    // Возвращает:
    //   false — обнаружено строгое противоречие (0 = 1, UNSAT)
    //   true — успешно приведено к RREF, новые унарные и бинарные равенства выгружены
    bool eliminate(std::vector<int>& var_alias, std::vector<std::pair<int, int>>& level0_units, int& out_substituted);

    size_t get_xor_count() const { return rows.size(); }
    void clear() { rows.clear(); }

private:
    int num_vars;
    size_t num_words;
    std::vector<XorRow> rows;
};
