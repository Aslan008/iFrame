#pragma once
#include <vector>
#include <random>
#include <utility>
#include <cstdint>
#include <cmath>
#include <immintrin.h>

struct alignas(16) Vec4 {
    union {
        struct { float x, y, z, w; };
        __m128 m;
    };

    inline Vec4() : m(_mm_setzero_ps()) {}
    inline Vec4(float x, float y, float z, float w) : m(_mm_set_ps(w, z, y, x)) {}
    inline Vec4(__m128 v) : m(v) {}

    inline Vec4 operator+(const Vec4& o) const {
        return Vec4(_mm_add_ps(m, o.m));
    }
    inline Vec4& operator+=(const Vec4& o) {
        m = _mm_add_ps(m, o.m);
        return *this;
    }
    inline Vec4& operator-=(const Vec4& o) {
        m = _mm_sub_ps(m, o.m);
        return *this;
    }
    inline Vec4 operator-(const Vec4& o) const {
        return Vec4(_mm_sub_ps(m, o.m));
    }
    inline Vec4 operator*(float s) const {
        return Vec4(_mm_mul_ps(m, _mm_set1_ps(s)));
    }
    inline float dot(const Vec4& o) const {
        __m128 r = _mm_dp_ps(m, o.m, 0xF1);
        return _mm_cvtss_f32(r);
    }
    inline float norm_sq() const {
        return dot(*this);
    }
    inline float norm() const {
        return std::sqrt(norm_sq());
    }
};

class PhysicsSimulator {
public:
    PhysicsSimulator(int num_vars, int dim = 4);
    
    void set_dimension(int d) { dim = (d == 1) ? 1 : 4; }
    int get_dimension() const { return dim; }

    void clear_race();
    void add_clause(const std::vector<int>& unresolved_lits);
    void add_clause_fast(const int* unresolved_lits, int len);
    
    // Холодная гонка: сброс позиций из saved_positions, max_steps шагов, ранжирование
    std::vector<std::pair<int, int>> pick_ranking(int max_steps, const std::vector<double>& saved_positions);
    
    // Тёплая (инкрементальная) гонка: состояние живёт между вызовами,
    // steps шагов интегрирования, аккумуляторы фрустрации затухают на decay
    std::vector<std::pair<int, int>> pick_ranking_warm(int steps, double decay = 0.75);
    
    // Полный сброс состояния (позиции ~0.5, скорости 0, аккумуляторы 0)
    void reset_state();
  
    // План D: Извлечение скрытых корреляций (4D косинусный спектральный анализ)
    // Возвращает список эквивалентностей: {var1, var2} => var1 == var2, {var1, -var2} => var1 != var2
    std::vector<std::pair<int, int>> extract_correlations();

    // Фаза 3: Спектральная 4D-кластеризация графа на сфере S^3
    // Возвращает вектор кластеров для всех переменных: cluster_id in [0..num_clusters-1]
    std::vector<int> compute_4d_clusters(int num_clusters = 4, int relax_steps = 48);

    // Финальные 4D позиции переменных после последней гонки (для write-back в Solver)
    const std::vector<Vec4>& get_positions() const { return positions; }

    // Контрольные точки (Phase-Space Checkpointing) и квантовое туннелирование
    void save_checkpoint();
    void restore_checkpoint();
    void apply_conflict_tunneling(const std::vector<int>& conflict_lits, float impulse_magnitude = 0.5f);
    bool has_checkpoint() const { return checkpoint_valid; }

    // 4D Probe Ray-Tracing: зондирование энергетического ландшафта вокруг частиц
    // и применение предиктивного импульса выхода из потенциальных ям
    void apply_probe_ray_tracing(float probe_step = 0.15f, float escape_kick = 0.35f);

    // 4D Raycast Hoverboard Suspension: левитация над барьерами и скольжение по касательной
    void apply_hoverboard_suspension(float spring_k = 0.35f, float damping_c = 0.15f);

    // 4D Tunnel Ray & Teleportation: глубокое пробивание стен насквозь и мгновенная квантовая телепортация
    void apply_tunnel_teleportation(float deep_tunnel_dist = 0.65f, float min_energy_gain = 0.10f);

    // 4D Ground Sonar & Gravity Anchor: луч сонара строго вниз для обнаружения и захвата пропущенных русел
    void apply_ground_sonar_anchor(float basin_threshold = 0.04f, float snap_strength = 0.85f);

    // Подключение генома адаптивной эволюции к физическим симуляциям
    void set_evolution_params(float spring_k, float damping_c, float deep_dist, float min_gain) {
        evo_spring_k = spring_k;
        evo_damping_c = damping_c;
        evo_deep_tunnel_dist = deep_dist;
        evo_min_energy_gain = min_gain;
    }

private:
    float evo_spring_k = 0.30f;
    float evo_damping_c = 0.10f;
    float evo_deep_tunnel_dist = 0.65f;
    float evo_min_energy_gain = 0.10f;
    double integrate_steps(int nsteps, float friction = 0.88f, bool record_explosion = false);
    std::vector<std::pair<int, int>> rank_active();

private:
    int num_vars;
    int dim; // 1 или 4
    
    // Плоские буферы для избежания аллокаций
    std::vector<int> flat_vars;         // |lit| — индекс переменной
    std::vector<float> flat_signs;      // знак литерала (+1 / -1) для безветвленных вычислений
    std::vector<int> clause_starts;
    std::vector<int> clause_lens;
    std::vector<float> clause_weights;
    std::vector<float> clause_stiffness;
    std::vector<Vec4> clause_lateral_dirs; // Детерминированные ортогональные векторы клауз в 4D
    std::vector<std::vector<int>> var_clauses; // Список смежных клауз для Ray-Tracing зондирования
    
    std::vector<Vec4> forces;
    std::vector<Vec4> positions;
    std::vector<Vec4> velocities;
    std::vector<double> cross_scores;     // пересечения 0.5 (в тёплом режиме — затухающий счётчик)
    std::vector<double> energy_scores;    // сумма кинетической энергии
    std::vector<double> vibration_scores; // количество смен направления скорости
    std::vector<double> explosion_scores; // амплитуда кинетического импульса
    std::vector<float> equators;          // локальные экваторы (по оси x)
    std::vector<int> latest_decisions;
    
    std::vector<int> active_vars;
    std::vector<bool> seen_vars;
    
    std::mt19937 gen;
    std::mt19937 gen_yzw; // Отдельный поток шума yzw: не сдвигает x-шум (A/B-гигиена)

    // Хранилище контрольной точки (Phase-Space Checkpoint)
    std::vector<Vec4> checkpoint_positions;
    std::vector<Vec4> checkpoint_velocities;
    bool checkpoint_valid = false;
};
