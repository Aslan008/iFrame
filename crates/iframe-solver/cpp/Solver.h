#pragma once
#include <vector>
#include <unordered_set>
#include "Physics.h"
#include "Gauss.h"
#include "XOREngine.h"
#include "AdderEngine.h"
#include "Evolution.h"

struct CDCLClause {
    std::vector<int> lits;
    bool learned;
    int lbd;
    bool deleted;
};

// Результат решения: SAT — найдена модель, UNSAT — доказана невыполнимость,
// UNKNOWN — превышен лимит конфликтов (ответа нет)
enum class SolveResult { SAT, UNSAT, UNKNOWN };

class CDCLSolver {
public:
    CDCLSolver(int num_vars, const std::vector<std::vector<int>>& initial_clauses, int max_conflicts = 10000, int sim_steps = 18,
               int physics_max_lbd = 3, int race_every = 4, int dim = 4);
    CDCLSolver(int num_vars, const std::vector<std::vector<int>>& initial_clauses, const std::vector<int>& assumptions, int max_conflicts = 10000, int sim_steps = 18,
               int physics_max_lbd = 3, int race_every = 4, int dim = 4);
    ~CDCLSolver();
    
    SolveResult solve();

    // Тестовый вход (--equiv-test): UP-верификация конкретной пары переменных
    // на загруженной формуле. same=true: v1<->v2; false: v1<->!v2.
    bool debug_verify_equivalence(int v1, int v2, bool same);

    int conflicts;
    int decisions_count;
    int restarts;
    bool ok;
    long long propagations;   // число обработанных назначений в BCP
    long long cache_hits;     // решений, взятых из кэша ранжирования
    double physics_time;      // суммарное время в эвристике ветвления, сек
    double lbd_ema = 3.0;     // экспоненциальное скользящее среднее LBD выученных клозов

    // План D + Фаза 1 (UP-верификация): статистика по инжектам
    long long plan_d_candidates = 0;  // пар, предложенных физикой
    long long plan_d_verified = 0;    // пар, ДОКАЗАННЫХ юнит-пропагацией
    long long plan_d_injected = 0;    // влитых клозов (2 на пару)
    long long substituted_vars = 0;   // перманентно устраненных переменных через Inprocessing
    
    // Фаза 2: Статистика модуля Гаусса (F_2) и BIG
    long long gauss_xors_found = 0;
    long long gauss_units_found = 0;
    long long gauss_equivs_found = 0;
    long long big_equivs_found = 0;
    long long xor_propagations = 0;
    long long xor_conflicts = 0;

    // Intel-style Carry & Adder Engine
    long long full_adders_found = 0;
    long long half_adders_found = 0;
    long long carry_propagations = 0;
    long long carry_conflicts = 0;

    // Фаза 5: Vivification & BVE
    long long vivified_clauses = 0;
    long long vivified_lits_removed = 0;
    long long bve_eliminated_vars = 0;
    
    // Режимы (настраиваются после конструирования)
    bool vsids_mode = false;       // ветвление по VSIDS вместо физики
    int warm_steps = 0;             // >0: тёплая (инкрементальная) физика, N шагов на решение
    bool luby_restarts = true;      // Luby-рестарты (иначе геометрические ×1.5)
    bool minimize_learned = true;  // минимизация выученных клозов (self-subsumption)
    double var_decay = 0.95;       // затухание VSIDS-активностей
    
    // Фаза 3: Спектральная 4D-кластеризация
    int num_clusters = 4;

    // Фаза 4: Многопоточный обмен клозами и кооперация
    class SharedClausePool* shared_pool = nullptr;
    int thread_id = 0;
    size_t shared_read_idx = 0;
    size_t shared_equiv_read_idx = 0;
    size_t shared_unit_read_idx = 0;
    long long shared_equivs_imported = 0;
    long long shared_units_imported = 0;
    long long shared_claims_skipped = 0;
    int max_cube_depth = 12; // Глубина нарезки и бронирования кубов (Guiding Paths)
    
    // Вектор результатов: 0=UNASSIGNED, 1=TRUE, -1=FALSE
    std::vector<int> assigns; 

private:
    int num_vars;
    int max_conflicts;
    int sim_steps;
    int conflicts_since_restart;
    int restart_limit;
    
    int physics_max_lbd;   // максимальный LBD выученных клозов, попадающих в физику
    int race_every;        // сколько решений обслуживает одна гонка (1 = гонка на каждое решение)
    int dim;               // размерность физики (1 или 4)
    
    int next_reduce_conflict;
    int reduce_increment;
    
    // Кэш ранжирования физической гонки (амортизация)
    std::vector<std::pair<int, int>> race_cache;
    size_t race_cache_pos;
    int race_uses;
    
    // VSIDS
    std::vector<double> activity;
    double var_inc;
    
    // Используем сырые указатели вместо shared_ptr для устранения оверхеда
    std::vector<CDCLClause*> clauses;
    std::vector<CDCLClause*> original_clauses;
    std::vector<CDCLClause*> learned_clauses;
    
struct Watcher {
    CDCLClause* clause;
    int blocker;
};

// watches[lit_idx]
std::vector<std::vector<Watcher>> watches;
    
    std::vector<int> decision_level;
    std::vector<CDCLClause*> reason;
    
    std::vector<int> trail;
    std::vector<int> trail_lim;
    int qhead;
    
    std::vector<int> saved_phases;
    std::vector<int> target_phases;
    int best_trail_size = 0;
    std::vector<double> saved_positions;
    std::vector<int> last_conflict_lits;
    
    // Для O(1) анализа конфликтов без std::set
    std::vector<bool> seen;
    std::vector<int> seen_level; 

    // Пары, чья эквивалентность уже доказана и влита (dedup против дублей)
    std::unordered_set<long long> plan_d_done;
    
    // Inprocessing / Union-Find подстановка
    std::vector<int> var_alias; // 0 = root, иначе signed literal родителя
    int find_root_lit(int lit);
    bool apply_substitutions();
    void reconstruct_model();
    
    // Tier-1: Binary Implication Graph (BIG) + Tarjan SCC
    bool run_big_inprocessing();

    // Фаза 2: Гауссов инпроцессинг над F_2
    bool run_gauss_inprocessing();

    // BVE Preprocessing (Bounded Variable Elimination с защитой XOR)
    struct BVEWitness {
        int var;
        std::vector<std::vector<int>> pos_clauses;
    };
    std::vector<BVEWitness> bve_stack;
    std::vector<bool> is_eliminated;
    bool run_bve_preprocessing();

    // Фаза 3: Спектральная 4D-кластеризация графа формулы
    std::vector<int> var_cluster;
    void initialize_spectral_clusters();
    
    // Персистентный симулятор для переиспользования буферов
    PhysicsSimulator sim; 
    
    // Tier-1 On-the-fly XOR Engine в трейле
    XOREngine xor_engine; 
    
    // Intel-style Carry & Adder Engine
    AdderEngine adder_engine; 

    // Модуль динамической самоэволюции правил и параметров
    SelfEvolutionEngine evolution; 
    
    static int lit_idx(int lit) {
        return (lit > 0) ? (lit << 1) : (((-lit) << 1) | 1);
    }
    
    void attach_clause(CDCLClause* clause);
    void detach_clause(CDCLClause* clause);
    inline int lit_val(int lit) const {
        int a = assigns[std::abs(lit)];
        return (lit > 0) ? a : -a;
    }
    int current_level();
    void new_decision_level();
    void assign_lit(int lit, CDCLClause* r);
    void backjump(int target_level);

    // Фаза 5: Vivification
    void vivify_clauses();

    // Фаза 1 (soundness): пробная UP-пропагация пары допущений на временном
    // уровне с полным откатом. true <=> база ∧ lit1 ∧ lit2 противоречива.
    bool up_conflict(int lit1, int lit2);
    // Доказать v1<->v2 (same=true) или v1<->!v2 (same=false) юнит-пропагацией.
    bool verify_equivalence(int v1, int v2, bool same);

    CDCLClause* propagate();
    
    // returns (learned_lits, backjump_lvl), lbd is output via ref
    std::pair<std::vector<int>, int> analyze_conflict(CDCLClause* conflict_clause, int& out_lbd);
    void reduceDB();

    // Memory Pool / Recycling для CDCLClause (устранение malloc/free и фрагментации)
    std::vector<CDCLClause*> clause_pool;
    CDCLClause* alloc_clause(const std::vector<int>& lits, bool learned, int lbd);
    void free_clause(CDCLClause* c);

    std::pair<int, int> pick_branch_lit();
    std::pair<int, int> validate_branch_with_shared_cubes(std::pair<int, int> branch);
};
