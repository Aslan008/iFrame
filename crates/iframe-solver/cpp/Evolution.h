#pragma once
#include <vector>
#include <cmath>
#include <algorithm>
#include <iostream>

struct EvolutionStats {
    long long total_decisions = 0;
    long long total_conflicts = 0;
    long long total_propagations = 0;
    long long total_teleports = 0;
    long long total_hover_bounces = 0;
    double avg_lbd = 2.0;
    int generation = 0;
};

struct EvolutionGenome {
    // 1. Физические гены (параметры 4D-симуляции)
    float spring_k = 0.35f;           // Жесткость пружин ховерборда [0.15 .. 0.85]
    float damping_c = 0.15f;          // Вязкость демпфера [0.05 .. 0.40]
    float deep_tunnel_dist = 0.65f;   // Дальность туннельного луча [0.35 .. 0.95]
    float min_energy_gain = 0.10f;    // Порог квантовой телепортации [0.02 .. 0.30]
    float friction = 0.88f;           // Трение в 4D-пространстве [0.75 .. 0.96]

    // 2. Логические гены CDCL
    double var_decay = 0.95;          // Скорость затухания VSIDS [0.85 .. 0.99]
    int warm_steps = 4;               // Шаги физики на развилку [2 .. 12]
    int restart_base = 100;           // Базовый лимит рестартов [50 .. 300]
    int physics_max_lbd = 3;          // Фильтр качества LBD для физики [2 .. 6]
    
    // Фитнес текущего генома
    double fitness = 0.0;
};

class SelfEvolutionEngine {
public:
    explicit SelfEvolutionEngine(bool enabled = true) : enabled(enabled) {}

    bool is_enabled() const { return enabled; }
    void set_enabled(bool e) { enabled = e; }

    const EvolutionGenome& get_genome() const { return active_genome; }
    EvolutionGenome& get_mutable_genome() { return active_genome; }
    const EvolutionStats& get_stats() const { return cur_stats; }

    // Регистрация событий в реальном времени
    void record_decision() { cur_stats.total_decisions++; }
    void record_conflict(int lbd) {
        cur_stats.total_conflicts++;
        double alpha = 0.05;
        cur_stats.avg_lbd = (1.0 - alpha) * cur_stats.avg_lbd + alpha * lbd;
    }
    void record_propagation() { cur_stats.total_propagations++; }
    void record_teleport() { cur_stats.total_teleports++; }
    void record_hover_bounce() { cur_stats.total_hover_bounces++; }

    // Эволюционный шаг адаптации (вызывается на рестартах)
    bool evolve_generation(int current_conflicts) {
        if (!enabled || cur_stats.total_decisions < 30) return false;

        cur_stats.generation++;
        
        // 1. Вычисление метрики эффективности (Fitness function):
        // Высокий фитнес = много пропагаций на решение, мало конфликтов, низкий LBD
        double props_per_dec = (cur_stats.total_decisions > 0) ? 
            static_cast<double>(cur_stats.total_propagations) / cur_stats.total_decisions : 1.0;
        double conflict_ratio = (cur_stats.total_decisions > 0) ?
            static_cast<double>(cur_stats.total_conflicts) / cur_stats.total_decisions : 0.5;
        double lbd_penalty = std::max(1.0, cur_stats.avg_lbd);

        double new_fitness = (props_per_dec * 2.0) / (conflict_ratio * 10.0 + lbd_penalty);

        prev_fitness = new_fitness;
        active_genome.fitness = new_fitness;

        // 2. Адаптивная мутация генов ховерборда
        double bounce_rate = (cur_stats.total_decisions > 0) ? 
            static_cast<double>(cur_stats.total_hover_bounces) / cur_stats.total_decisions : 0.0;
        if (bounce_rate > 0.35) {
            // Слишком много ударов о стены -> делаем пружины жестче
            active_genome.spring_k = std::min(0.85f, active_genome.spring_k * 1.08f);
            active_genome.damping_c = std::min(0.40f, active_genome.damping_c * 1.05f);
        } else if (bounce_rate < 0.05) {
            // Движение плавное -> смягчаем пружину для быстрой маневренности
            active_genome.spring_k = std::max(0.15f, active_genome.spring_k * 0.95f);
        }

        // 3. Адаптация квантового туннелирования
        double teleport_rate = (cur_stats.total_decisions > 0) ?
            static_cast<double>(cur_stats.total_teleports) / cur_stats.total_decisions : 0.0;
        if (teleport_rate > 0.15) {
            active_genome.deep_tunnel_dist = std::min(0.95f, active_genome.deep_tunnel_dist * 1.05f);
            active_genome.min_energy_gain = std::max(0.03f, active_genome.min_energy_gain * 0.95f);
        } else if (teleport_rate < 0.02) {
            active_genome.min_energy_gain = std::max(0.02f, active_genome.min_energy_gain * 0.90f);
        }

        // 4. Адаптация затухания VSIDS
        if (conflict_ratio > 0.40) {
            active_genome.var_decay = std::max(0.88, active_genome.var_decay * 0.98);
        } else {
            active_genome.var_decay = std::min(0.98, active_genome.var_decay * 1.01);
        }

        return true;
    }

private:
    bool enabled = true;
    EvolutionGenome active_genome;
    EvolutionStats cur_stats;
    double prev_fitness = 0.0;
};
