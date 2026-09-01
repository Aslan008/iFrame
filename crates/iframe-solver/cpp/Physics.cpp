#include "Physics.h"
#include <algorithm>
#include <cmath>

PhysicsSimulator::PhysicsSimulator(int num_vars, int dim)
    : num_vars(num_vars), dim(dim), gen(42), gen_yzw(1337),
      forces(num_vars + 1, Vec4(0.0f, 0.0f, 0.0f, 0.0f)),
      positions(num_vars + 1, Vec4(0.5f, 0.0f, 0.0f, 0.0f)),
      velocities(num_vars + 1, Vec4(0.0f, 0.0f, 0.0f, 0.0f)),
      cross_scores(num_vars + 1, 0.0), energy_scores(num_vars + 1, 0.0),
      vibration_scores(num_vars + 1, 0.0), explosion_scores(num_vars + 1, 0.0),
      equators(num_vars + 1, 0.5f), latest_decisions(num_vars + 1, -1),
      seen_vars(num_vars + 1, false), var_clauses(num_vars + 1) {
  // Резервируем память под буферы
  flat_vars.reserve(num_vars * 10);
  flat_signs.reserve(num_vars * 10);
  clause_starts.reserve(num_vars * 4);
  clause_lens.reserve(num_vars * 4);
  clause_weights.reserve(num_vars * 4);
  clause_stiffness.reserve(num_vars * 4);
  active_vars.reserve(num_vars);
  for (int v = 1; v <= num_vars; ++v) {
    var_clauses[v].reserve(8);
  }
}

void PhysicsSimulator::clear_race() {
  flat_vars.clear();
  flat_signs.clear();
  clause_starts.clear();
  clause_lens.clear();
  clause_weights.clear();
  clause_stiffness.clear();

  for (int v : active_vars) {
    var_clauses[v].clear();
    seen_vars[v] = false;
  }
  active_vars.clear();
}

void PhysicsSimulator::add_clause_fast(const int *unresolved_lits, int len) {
  int start = static_cast<int>(flat_vars.size());
  int cid = static_cast<int>(clause_starts.size());

  clause_starts.push_back(start);
  clause_lens.push_back(len);
  clause_weights.push_back(1.0f);

  float stiffness = 0.8f;
  if (len == 1)
    stiffness = 2.5f;
  else if (len == 2)
    stiffness = 1.4f;
  else if (len == 3)
    stiffness = 1.0f;
  clause_stiffness.push_back(stiffness);

  for (int i = 0; i < len; ++i) {
    int lit = unresolved_lits[i];
    int var = std::abs(lit);
    flat_vars.push_back(var);
    flat_signs.push_back(lit > 0 ? 1.0f : -1.0f);
    var_clauses[var].push_back(cid);
    if (!seen_vars[var]) {
      seen_vars[var] = true;
      active_vars.push_back(var);
    }
  }
}

void PhysicsSimulator::add_clause(const std::vector<int> &unresolved_lits) {
  add_clause_fast(unresolved_lits.data(), static_cast<int>(unresolved_lits.size()));
}

double PhysicsSimulator::integrate_steps(int nsteps, float friction,
                                        bool record_explosion) {
  int num_clauses = static_cast<int>(clause_starts.size());
  double overall_max_velocity = 0.0;

  if (dim == 1) {
    // ------------------- 1D КЛАССИЧЕСКИЙ РЕЖИМ -------------------
    float gravity_to_center_x = 0.015f;
    for (int step = 0; step < nsteps; ++step) {
      for (int var : active_vars) {
        forces[var].x = 0.0f;
      }

      for (int i = 0; i < num_clauses; ++i) {
        int start = clause_starts[i];
        int len = clause_lens[i];

        float max_val = 0.0f;
        for (int k = 0; k < len; ++k) {
          float val = 0.5f + flat_signs[start + k] *
                                 (positions[flat_vars[start + k]].x - 0.5f);
          if (val > max_val) {
            max_val = val;
          }
        }

        if (max_val < 0.6f) {
          float stress = 0.6f - max_val;
          float barrier_force = 0.03f / (max_val + 0.12f);
          clause_weights[i] *= (1.0f + 0.03f * stress);

          float eff_force = (0.04f + stress + barrier_force) * clause_weights[i] *
                            clause_stiffness[i];

          for (int k = 0; k < len; ++k) {
            forces[flat_vars[start + k]].x += eff_force * flat_signs[start + k];
          }
        }
      }

      for (int var : active_vars) {
        float eq = equators[var];
        float central_gravity_x = (eq - positions[var].x) * gravity_to_center_x;

        float old_vel_x = velocities[var].x;
        velocities[var].x =
            velocities[var].x * friction + forces[var].x + central_gravity_x;

        float old_pos_x = positions[var].x;
        float new_pos_x = old_pos_x + velocities[var].x;

        bool crossed = ((old_pos_x < eq) && (new_pos_x >= eq)) ||
                       ((old_pos_x > eq) && (new_pos_x <= eq));
        if (crossed) {
          cross_scores[var] += 1.0;
          energy_scores[var] += std::abs(velocities[var].x);
          latest_decisions[var] = (velocities[var].x > 0.0f) ? 1 : 0;
        }

        if (old_vel_x * velocities[var].x < 0.0f) {
          vibration_scores[var] += 1.0;
        }

        float v_mag = std::abs(velocities[var].x);
        if (record_explosion) {
          float deviation = std::abs(new_pos_x - eq) + v_mag * 1.5f;
          if (deviation > explosion_scores[var]) {
            explosion_scores[var] = deviation;
          }
        }

        if (new_pos_x < 0.0f) new_pos_x = 0.0f;
        else if (new_pos_x > 1.0f) new_pos_x = 1.0f;
        positions[var].x = new_pos_x;

        if (v_mag > overall_max_velocity) {
          overall_max_velocity = v_mag;
        }
      }
    }
  } else {
    // ------------------- ИСТИННЫЙ 4D РЕЖИМ (Сфера S^3 / Вариант B) -------------------
    const Vec4 T(1.0f, 0.0f, 0.0f, 0.0f); // Глобальный вектор Истины

    for (int step = 0; step < nsteps; ++step) {
      for (int var : active_vars) {
        forces[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      }

      // 1. Вычисление 4D сил клауз
      for (int i = 0; i < num_clauses; ++i) {
        int start = clause_starts[i];
        int len = clause_lens[i];

        // Удовлетворенность литерала: val = sign * (P_v · T) = sign * P_v.x ∈ [-1, 1]
        float max_val = -2.0f;
        int best_k = 0;
        for (int k = 0; k < len; ++k) {
          float val = flat_signs[start + k] * positions[flat_vars[start + k]].x;
          if (val > max_val) {
            max_val = val;
            best_k = k;
          }
        }

        // Клауза фрустрирована, если все её литералы далеки от истины (max_val < 0.2)
        if (max_val < 0.2f) {
          float stress = 0.2f - max_val;
          float barrier_force = 0.04f / (max_val + 1.15f);
          clause_weights[i] *= (1.0f + 0.03f * stress);

          float eff_force = (0.05f + stress + barrier_force) * clause_weights[i] *
                            clause_stiffness[i];

          // 1) Притяжение наиболее близкого литерала к истине T
          int best_v = flat_vars[start + best_k];
          float best_sign = flat_signs[start + best_k];
          forces[best_v] += T * (eff_force * best_sign);

          // 2) Взаимное расталкивание по ортогональным осям (развязка тупиков в R^4)
          if (len >= 2) {
            for (int k1 = 0; k1 < len; ++k1) {
              for (int k2 = k1 + 1; k2 < len; ++k2) {
                int v1 = flat_vars[start + k1];
                int v2 = flat_vars[start + k2];
                // Ортогональная составляющая расталкивания
                Vec4 diff = positions[v1] - positions[v2];
                diff.x = 0.0f; // расталкиваем чисто по y, z, w
                float d_norm = diff.norm();
                if (d_norm > 1e-4f) {
                  Vec4 push = diff * (eff_force * 0.25f / d_norm);
                  forces[v1] += push;
                  forces[v2] -= push;
                }
              }
            }
          }
        }
      }

      // 2. Левитирующая подвеска ховерборда (Raycast Suspension) над барьерами с параметрами эволюции
      apply_hoverboard_suspension(evo_spring_k, evo_damping_c);

      // 3. Интегрирование скоростей и позиций с проекцией на 3-сферу S^3
      for (int var : active_vars) {
        // Затухание и ускорение
        velocities[var] = velocities[var] * friction + forces[var];

        // Обновление позиции
        Vec4 new_pos = positions[var] + velocities[var];
        float n = new_pos.norm();
        if (n < 1e-4f) {
          new_pos = T;
          n = 1.0f;
        }
        new_pos = new_pos * (1.0f / n); // Нормализация на единичную сферу S^3

        float old_x = positions[var].x;
        float new_x = new_pos.x;

        // Пересечение экватора x = 0 (точка неопределенности в S^3)
        bool crossed = ((old_x < 0.0f) && (new_x >= 0.0f)) ||
                       ((old_x > 0.0f) && (new_x <= 0.0f));
        if (crossed) {
          cross_scores[var] += 1.0;
          energy_scores[var] += velocities[var].norm();
          latest_decisions[var] = (new_x > 0.0f) ? 1 : 0;
        }

        if (old_x * new_x < 0.0f) {
          vibration_scores[var] += 1.0;
        }

        float v_mag = velocities[var].norm();
        if (record_explosion) {
          float deviation = std::abs(new_x) + v_mag * 1.5f;
          if (deviation > explosion_scores[var]) {
            explosion_scores[var] = deviation;
          }
        }

        positions[var] = new_pos;

        if (v_mag > overall_max_velocity) {
          overall_max_velocity = v_mag;
        }
      }
    }
    // 4D Probe Ray-Tracing: зондирование энергетического ландшафта и выход из ям
    apply_probe_ray_tracing(0.12f, 0.25f);
  }

  return overall_max_velocity;
}

std::vector<std::pair<int, int>> PhysicsSimulator::rank_active() {
  struct Entry {
    int var;
    int dec;
    double score;
  };
  std::vector<Entry> entries;
  entries.reserve(active_vars.size());

  for (int var : active_vars) {
    double c_count = cross_scores[var];
    double e_sum = energy_scores[var];
    double v_count = vibration_scores[var];
    double exp_score = explosion_scores[var];
    int dec = latest_decisions[var];
    if (dec == -1) {
      if (dim == 1) dec = (positions[var].x >= 0.5f) ? 1 : 0;
      else dec = (positions[var].x >= 0.0f) ? 1 : 0;
    }

    double centrality;
    if (dim == 1) {
      centrality = 0.5 - std::abs(positions[var].x - equators[var]);
    } else {
      centrality = 1.0 - std::abs(positions[var].x); // Близость к экватору S^3 (x=0)
    }

    double score = c_count * 10.0 + v_count * 2.0 + e_sum * 100.0 +
                   exp_score * 100.0 + centrality;
    entries.push_back({var, dec, score});
  }

  std::sort(entries.begin(), entries.end(),
            [](const Entry &a, const Entry &b) { return a.score > b.score; });

  std::vector<std::pair<int, int>> ranking;
  ranking.reserve(entries.size());
  for (const auto &e : entries) {
    ranking.push_back({e.var, e.dec});
  }
  return ranking;
}

std::vector<std::pair<int, int>>
PhysicsSimulator::pick_ranking(int max_steps,
                               const std::vector<double> &saved_positions) {
  if (clause_starts.empty()) {
    return {};
  }

  std::uniform_real_distribution<float> dist_x(-0.005f, 0.005f);
  std::uniform_real_distribution<float> dist_yzw(-0.05f, 0.05f);

  if (dim == 1) {
    for (int var = 1; var <= num_vars; ++var) {
      float p_x = 0.2f * static_cast<float>(saved_positions[var]) + 0.8f * 0.5f + dist_x(gen);
      if (p_x < 0.01f) p_x = 0.01f;
      if (p_x > 0.99f) p_x = 0.99f;

      positions[var] = Vec4(p_x, 0.0f, 0.0f, 0.0f);
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      cross_scores[var] = 0.0;
      energy_scores[var] = 0.0;
      vibration_scores[var] = 0.0;
      explosion_scores[var] = 0.0;
      equators[var] = 0.5f;
      latest_decisions[var] = -1;
    }
  } else {
    // 4D Spherical Spin Initialization
    for (int var = 1; var <= num_vars; ++var) {
      float p_x = static_cast<float>(saved_positions[var]) * 0.2f + dist_x(gen);
      Vec4 p(p_x, dist_yzw(gen_yzw), dist_yzw(gen_yzw), dist_yzw(gen_yzw));
      float n = p.norm();
      if (n < 1e-4f) p = Vec4(1.0f, 0.0f, 0.0f, 0.0f);
      else p = p * (1.0f / n);

      positions[var] = p;
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      cross_scores[var] = 0.0;
      energy_scores[var] = 0.0;
      vibration_scores[var] = 0.0;
      explosion_scores[var] = 0.0;
      equators[var] = 0.0f;
      latest_decisions[var] = -1;
    }
  }

  int phase1 = max_steps / 3;

  // Фаза 1: Сортировка (трение)
  integrate_steps(phase1, 0.88f, false);
  apply_tunnel_teleportation(evo_deep_tunnel_dist, evo_min_energy_gain);

  // Переход: установка локальных экваторов
  for (int var : active_vars) {
    equators[var] = positions[var].x;
  }

  // Фаза 2: Вязкое гашение (Viscous Quenching)
  int phase2_max = max_steps / 3;
  for (int i = 0; i < phase2_max; ++i) {
    double max_vel = integrate_steps(1, 0.1f, false);
    if (max_vel < 1e-4) {
      break;
    }
  }

  // Фиксация финального экватора и гравитационный захват сонаром
  for (int var : active_vars) {
    equators[var] = positions[var].x;
    velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
    explosion_scores[var] = 0.0;
  }
  apply_ground_sonar_anchor(0.04f, 0.85f);
  return rank_active();
}

std::vector<std::pair<int, int>>
PhysicsSimulator::pick_ranking_warm(int steps, double decay) {
  if (clause_starts.empty()) {
    return {};
  }

  for (int var = 1; var <= num_vars; ++var) {
    cross_scores[var] *= decay;
    energy_scores[var] *= decay;
    vibration_scores[var] *= decay;
    explosion_scores[var] *= decay;
  }

  // Туннельное сканирование сквозь стены и квантовая телепортация
  apply_tunnel_teleportation(evo_deep_tunnel_dist, evo_min_energy_gain);

  integrate_steps(steps, 0.88f, false);
  apply_ground_sonar_anchor(0.04f, 0.85f);
  return rank_active();
}

void PhysicsSimulator::reset_state() {
  std::uniform_real_distribution<float> dist_x(-0.005f, 0.005f);
  std::uniform_real_distribution<float> dist_yzw(-0.05f, 0.05f);

  if (dim == 1) {
    for (int var = 1; var <= num_vars; ++var) {
      positions[var] = Vec4(0.5f + dist_x(gen), 0.0f, 0.0f, 0.0f);
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      cross_scores[var] = 0.0;
      energy_scores[var] = 0.0;
      vibration_scores[var] = 0.0;
      explosion_scores[var] = 0.0;
      equators[var] = 0.5f;
      latest_decisions[var] = -1;
    }
  } else {
    for (int var = 1; var <= num_vars; ++var) {
      Vec4 p(dist_x(gen), dist_yzw(gen_yzw), dist_yzw(gen_yzw), dist_yzw(gen_yzw));
      float n = p.norm();
      if (n < 1e-4f) p = Vec4(1.0f, 0.0f, 0.0f, 0.0f);
      else p = p * (1.0f / n);

      positions[var] = p;
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      cross_scores[var] = 0.0;
      energy_scores[var] = 0.0;
      vibration_scores[var] = 0.0;
      explosion_scores[var] = 0.0;
      equators[var] = 0.0f;
      latest_decisions[var] = -1;
    }
  }
  checkpoint_valid = false;
}

std::vector<std::pair<int, int>> PhysicsSimulator::extract_correlations() {
  if (clause_starts.empty() || active_vars.size() < 2) return {};

  std::uniform_real_distribution<float> dist_x(-0.02f, 0.02f);
  std::uniform_real_distribution<float> dist_yzw(-0.05f, 0.05f);

  if (dim == 1) {
    for (int var = 1; var <= num_vars; ++var) {
      positions[var] = Vec4(0.5f + dist_x(gen), 0.0f, 0.0f, 0.0f);
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      equators[var] = 0.5f;
    }
  } else {
    for (int var = 1; var <= num_vars; ++var) {
      Vec4 p(dist_x(gen), dist_yzw(gen_yzw), dist_yzw(gen_yzw), dist_yzw(gen_yzw));
      float n = p.norm();
      if (n < 1e-4f) p = Vec4(1.0f, 0.0f, 0.0f, 0.0f);
      else p = p * (1.0f / n);
      positions[var] = p;
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      equators[var] = 0.0f;
    }
  }

  // 1. Динамический прогон + Гашение
  integrate_steps(36, 0.92f, false);
  integrate_steps(12, 0.30f, false);

  // 2. Расчет попарных корреляций
  struct Candidate {
    int v1;
    int v2;
    float sim;
  };
  std::vector<Candidate> candidates;

  const float POS_THRESHOLD = 0.88f;
  const float NEG_THRESHOLD = -0.88f;
  const size_t MAX_RAW_CANDIDATES = 500;

  if (dim == 1) {
    // 1D Корреляция по оси X (только сильные смещения > 0.35 от центра)
    for (size_t i = 0; i < active_vars.size() && candidates.size() < MAX_RAW_CANDIDATES; ++i) {
      int v1 = active_vars[i];
      float d1 = positions[v1].x - 0.5f;
      if (std::abs(d1) < 0.35f) continue;
      float s1 = (d1 > 0.0f) ? 1.0f : -1.0f;

      for (size_t j = i + 1; j < active_vars.size() && candidates.size() < MAX_RAW_CANDIDATES; ++j) {
        int v2 = active_vars[j];
        float d2 = positions[v2].x - 0.5f;
        if (std::abs(d2) < 0.35f) continue;
        float s2 = (d2 > 0.0f) ? 1.0f : -1.0f;

        float sim_val = std::abs(d1 * d2);
        if (s1 == s2) {
          candidates.push_back({v1, v2, sim_val});
        } else {
          candidates.push_back({v1, -v2, sim_val});
        }
      }
    }
  } else {
    // 4D Сферическое скалярное произведение (Естественное выравнивание на S^3)
    for (size_t i = 0; i < active_vars.size() && candidates.size() < MAX_RAW_CANDIDATES; ++i) {
      int v1 = active_vars[i];
      const Vec4& p1 = positions[v1];

      for (size_t j = i + 1; j < active_vars.size() && candidates.size() < MAX_RAW_CANDIDATES; ++j) {
        int v2 = active_vars[j];
        const Vec4& p2 = positions[v2];

        float dot = p1.dot(p2); // p1 и p2 уже единичные векторы на S^3
        if (dot >= POS_THRESHOLD) {
          candidates.push_back({v1, v2, dot});
        } else if (dot <= NEG_THRESHOLD) {
          candidates.push_back({v1, -v2, -dot});
        }
      }
    }
  }

  if (candidates.empty()) return {};

  // 3. Топ-K фильтрация (не более 50 наиболее уверенных кандидатов)
  std::sort(candidates.begin(), candidates.end(), [](const Candidate& a, const Candidate& b) {
    return a.sim > b.sim;
  });

  size_t max_pairs = std::min(candidates.size(), static_cast<size_t>(50));
  std::vector<std::pair<int, int>> equivalents;
  equivalents.reserve(max_pairs);

  for (size_t i = 0; i < max_pairs; ++i) {
    equivalents.push_back({candidates[i].v1, candidates[i].v2});
  }

  return equivalents;
}

std::vector<int> PhysicsSimulator::compute_4d_clusters(int num_clusters, int relax_steps) {
  std::vector<int> clusters(num_vars + 1, 0);
  if (num_clusters <= 1 || clause_starts.empty() || active_vars.empty()) {
    return clusters;
  }

  // 1. Инициализируем сферическое распределение
  std::uniform_real_distribution<float> dist_x(-0.02f, 0.02f);
  std::uniform_real_distribution<float> dist_yzw(-0.05f, 0.05f);

  for (int var = 1; var <= num_vars; ++var) {
    Vec4 p(dist_x(gen), dist_yzw(gen_yzw), dist_yzw(gen_yzw), dist_yzw(gen_yzw));
    float n = p.norm();
    if (n < 1e-4f) p = Vec4(1.0f, 0.0f, 0.0f, 0.0f);
    else p = p * (1.0f / n);
    positions[var] = p;
    velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
  }

  // 2. Спектральная релаксация графа на сфере S^3
  integrate_steps(relax_steps, 0.90f, false);
  integrate_steps(16, 0.20f, false);

  // 3. Spherical K-Means кластеризация
  int K = std::min(num_clusters, static_cast<int>(active_vars.size()));
  std::vector<Vec4> centroids(K);

  // Ортогональные центроиды в R^4
  std::vector<Vec4> basis = {
      Vec4(1.0f, 0.0f, 0.0f, 0.0f),
      Vec4(-1.0f, 0.0f, 0.0f, 0.0f),
      Vec4(0.0f, 1.0f, 0.0f, 0.0f),
      Vec4(0.0f, -1.0f, 0.0f, 0.0f),
      Vec4(0.0f, 0.0f, 1.0f, 0.0f),
      Vec4(0.0f, 0.0f, -1.0f, 0.0f),
      Vec4(0.0f, 0.0f, 0.0f, 1.0f),
      Vec4(0.0f, 0.0f, 0.0f, -1.0f)
  };

  for (int k = 0; k < K; ++k) {
    centroids[k] = basis[k % basis.size()];
  }

  std::vector<int> counts(K, 0);

  for (int iter = 0; iter < 12; ++iter) {
    for (int var : active_vars) {
      const Vec4& p = positions[var];
      float best_dot = -2.0f;
      int best_k = 0;
      for (int k = 0; k < K; ++k) {
        float dot = p.dot(centroids[k]);
        if (dot > best_dot) {
          best_dot = dot;
          best_k = k;
        }
      }
      clusters[var] = best_k;
    }

    std::vector<Vec4> new_centroids(K, Vec4(0.0f, 0.0f, 0.0f, 0.0f));
    std::fill(counts.begin(), counts.end(), 0);

    for (int var : active_vars) {
      int k = clusters[var];
      new_centroids[k] += positions[var];
      counts[k]++;
    }

    for (int k = 0; k < K; ++k) {
      float n = new_centroids[k].norm();
      if (n > 1e-4f) {
        centroids[k] = new_centroids[k] * (1.0f / n);
      }
    }
  }

  // 4. Сортировка кластеров по проекции на ось X (топологический порядок)
  struct ClusterMeta {
    int old_id;
    float avg_x;
  };
  std::vector<ClusterMeta> meta(K);
  for (int k = 0; k < K; ++k) {
    meta[k].old_id = k;
    meta[k].avg_x = centroids[k].x;
  }
  std::sort(meta.begin(), meta.end(), [](const ClusterMeta& a, const ClusterMeta& b) {
    return a.avg_x > b.avg_x;
  });

  std::vector<int> rank_map(K, 0);
  for (int r = 0; r < K; ++r) {
    rank_map[meta[r].old_id] = r;
  }

  for (int var : active_vars) {
    clusters[var] = rank_map[clusters[var]];
  }

  return clusters;
}

void PhysicsSimulator::save_checkpoint() {
  checkpoint_positions = positions;
  checkpoint_velocities = velocities;
  checkpoint_valid = true;
}

void PhysicsSimulator::restore_checkpoint() {
  if (!checkpoint_valid) return;
  positions = checkpoint_positions;
  velocities = checkpoint_velocities;
}

void PhysicsSimulator::apply_conflict_tunneling(const std::vector<int>& conflict_lits, float impulse_magnitude) {
  if (!checkpoint_valid) return;
  
  // 1. Возвращаем частицы в проверенную геометрию наименьшей фрустрации
  positions = checkpoint_positions;
  velocities = checkpoint_velocities;

  // 2. Прикладываем ортогональный квантовый импульс туннелирования к переменным конфликта
  for (int lit : conflict_lits) {
    int v = std::abs(lit);
    if (v >= 1 && v <= num_vars) {
      // Реверс скорости по оси истинности x с коэффициентом отталкивания
      velocities[v].x = -velocities[v].x * 1.5f;

      // Ортогональный импульс в подпространстве (y, z, w) для увода в альтернативный квадрант
      float sign = (lit > 0) ? 1.0f : -1.0f;
      velocities[v].y += impulse_magnitude * sign * ((v % 2 == 0) ? 1.0f : -0.7f);
      velocities[v].z += impulse_magnitude * sign * ((v % 3 == 0) ? -1.0f : 0.8f);
      velocities[v].w += impulse_magnitude * sign * ((v % 5 == 0) ? 1.2f : -0.9f);

      // Сдвигаем позицию и проецируем на 3-сферу S^3
      positions[v] = positions[v] + velocities[v] * 0.1f;
      float n = positions[v].norm();
      if (n > 1e-4f) {
        positions[v] = positions[v] * (1.0f / n);
      }
    }
  }
}

void PhysicsSimulator::apply_probe_ray_tracing(float probe_step, float escape_kick) {
  if (dim != 4 || clause_starts.empty() || active_vars.empty()) return;

  // 8 базисных направлений зондирующих лучей в R^4
  static const Vec4 probe_rays[8] = {
      Vec4( 1.0f,  0.0f,  0.0f,  0.0f), // +X (истина)
      Vec4(-1.0f,  0.0f,  0.0f,  0.0f), // -X (ложь)
      Vec4( 0.0f,  1.0f,  0.0f,  0.0f), // +Y (ортогональ 1)
      Vec4( 0.0f, -1.0f,  0.0f,  0.0f), // -Y
      Vec4( 0.0f,  0.0f,  1.0f,  0.0f), // +Z (ортогональ 2)
      Vec4( 0.0f,  0.0f, -1.0f,  0.0f), // -Z
      Vec4( 0.0f,  0.0f,  0.0f,  1.0f), // +W (ортогональ 3)
      Vec4( 0.0f,  0.0f,  0.0f, -1.0f)  // -W
  };

  for (int var : active_vars) {
    const auto& c_indices = var_clauses[var];
    if (c_indices.empty()) continue;

    // Зондируем ТОЛЬКО частицы, которые застряли (низкая кинетическая энергия)
    if (velocities[var].norm() > 0.05f) continue;

    Vec4 orig_pos = positions[var];

    // 1. Вычисление базовой локальной энергии фрустрации
    float base_energy = 0.0f;
    for (int cid : c_indices) {
      int start = clause_starts[cid];
      int len = clause_lens[cid];
      float max_val = -2.0f;
      for (int k = 0; k < len; ++k) {
        float val = flat_signs[start + k] * positions[flat_vars[start + k]].x;
        if (val > max_val) max_val = val;
      }
      if (max_val < 0.2f) {
        base_energy += (0.2f - max_val) * clause_weights[cid];
      }
    }

    // 2. Зондирование 8 лучами вокруг частицы
    float min_ray_energy = 1e9f;
    int best_ray_idx = -1;
    int worse_count = 0;

    for (int r = 0; r < 8; ++r) {
      Vec4 probe_p = orig_pos + probe_rays[r] * probe_step;
      float n = probe_p.norm();
      if (n > 1e-4f) probe_p = probe_p * (1.0f / n);

      // Оцениваем энергию смежных клозов с виртуальной позиции луча
      float ray_energy = 0.0f;
      for (int cid : c_indices) {
        int start = clause_starts[cid];
        int len = clause_lens[cid];
        float max_val = -2.0f;
        for (int k = 0; k < len; ++k) {
          int v = flat_vars[start + k];
          float px = (v == var) ? probe_p.x : positions[v].x;
          float val = flat_signs[start + k] * px;
          if (val > max_val) max_val = val;
        }
        if (max_val < 0.2f) {
          ray_energy += (0.2f - max_val) * clause_weights[cid];
        }
      }

      if (ray_energy < min_ray_energy) {
        min_ray_energy = ray_energy;
        best_ray_idx = r;
      }
      if (ray_energy >= base_energy - 1e-4f) {
        worse_count++;
      }
    }

    // 3. Анализ рельефа:
    if (worse_count == 8 && base_energy > 0.01f) {
      // Частица на дне потенциальной ямы (локальный тупик фрустрации)!
      // Применяем предиктивный импульс выхода по направлению наименьшего подъема (седловой перевал)
      if (best_ray_idx >= 0) {
        velocities[var] += probe_rays[best_ray_idx] * escape_kick;
      }
    } else if (best_ray_idx >= 0 && min_ray_energy < base_energy) {
      // Есть выраженный спуск: придаем направляющее ускорение
      velocities[var] += probe_rays[best_ray_idx] * (escape_kick * 0.5f);
    }
  }
}

void PhysicsSimulator::apply_hoverboard_suspension(float spring_k, float damping_c) {
  if (dim != 4 || clause_starts.empty() || active_vars.empty()) return;

  for (int var : active_vars) {
    const auto& c_indices = var_clauses[var];
    if (c_indices.empty()) continue;

    Vec4 v = velocities[var];
    float v_norm = v.norm();
    if (v_norm < 1e-4f) continue;

    Vec4 v_dir = v * (1.0f / v_norm);
    Vec4 p = positions[var];

    // 1. Зондирующий луч ховерборда вперед по курсу движения
    float lookahead = 0.12f;
    Vec4 probe_p = p + v_dir * lookahead;
    float n = probe_p.norm();
    if (n > 1e-4f) probe_p = probe_p * (1.0f / n);

    // 2. Оценка энергии барьера впереди по курсу
    float ahead_energy = 0.0f;
    for (int cid : c_indices) {
      int start = clause_starts[cid];
      int len = clause_lens[cid];
      float max_val = -2.0f;
      for (int k = 0; k < len; ++k) {
        int v_idx = flat_vars[start + k];
        float px = (v_idx == var) ? probe_p.x : positions[v_idx].x;
        float val = flat_signs[start + k] * px;
        if (val > max_val) max_val = val;
      }
      if (max_val < 0.2f) {
        ahead_energy += (0.2f - max_val) * clause_weights[cid];
      }
    }

    // 3. Если впереди приближается барьер противоречия (ahead_energy > 0.05):
    if (ahead_energy > 0.05f) {
      // Нормаль отталкивания ховерборда (против направления удара о стену)
      Vec4 repulsion_dir = v_dir * -1.0f;

      // Сила упругой левитации (пружина + демпфер)
      float spring_compression = ahead_energy / (ahead_energy + 0.3f);
      float hover_force = spring_k * spring_compression - damping_c * v_norm;
      if (hover_force > 0.0f) {
        forces[var] += repulsion_dir * hover_force;
      }

      // Поворот по касательной: отклоняем вектор скорости в ортогональное подпространство (y, z, w),
      // чтобы частица не теряла скорость, а огибала барьер по касательной
      Vec4 tangent_glide(0.0f, v.y * 1.15f + ((var % 2 == 0) ? 0.03f : -0.03f),
                               v.z * 1.15f + ((var % 3 == 0) ? 0.03f : -0.03f),
                               v.w * 1.15f + ((var % 5 == 0) ? 0.03f : -0.03f));
      velocities[var] = velocities[var] * 0.85f + tangent_glide;
    }
  }
}

void PhysicsSimulator::apply_tunnel_teleportation(float deep_tunnel_dist, float min_energy_gain) {
  if (dim != 4 || clause_starts.empty() || active_vars.empty()) return;

  // 16 глубоких туннельных лучей (пробивание стен насквозь в R^4)
  static const Vec4 tunnel_rays[16] = {
      Vec4( 1.0f,  0.0f,  0.0f,  0.0f), Vec4(-1.0f,  0.0f,  0.0f,  0.0f),
      Vec4( 0.0f,  1.0f,  0.0f,  0.0f), Vec4( 0.0f, -1.0f,  0.0f,  0.0f),
      Vec4( 0.0f,  0.0f,  1.0f,  0.0f), Vec4( 0.0f,  0.0f, -1.0f,  0.0f),
      Vec4( 0.0f,  0.0f,  0.0f,  1.0f), Vec4( 0.0f,  0.0f,  0.0f, -1.0f),
      Vec4( 0.5f,  0.5f,  0.5f,  0.5f), Vec4(-0.5f,  0.5f,  0.5f,  0.5f),
      Vec4( 0.5f, -0.5f,  0.5f,  0.5f), Vec4( 0.5f,  0.5f, -0.5f,  0.5f),
      Vec4( 0.5f,  0.5f,  0.5f, -0.5f), Vec4(-0.5f, -0.5f,  0.5f,  0.5f),
      Vec4(-0.5f,  0.5f, -0.5f,  0.5f), Vec4(-0.5f,  0.5f,  0.5f, -0.5f)
  };

  for (int var : active_vars) {
    const auto& c_indices = var_clauses[var];
    if (c_indices.empty()) continue;

    Vec4 orig_pos = positions[var];

    float curr_energy = 0.0f;
    for (int cid : c_indices) {
      int start = clause_starts[cid];
      int len = clause_lens[cid];
      float max_val = -2.0f;
      for (int k = 0; k < len; ++k) {
        float val = flat_signs[start + k] * positions[flat_vars[start + k]].x;
        if (val > max_val) max_val = val;
      }
      if (max_val < 0.2f) {
        curr_energy += (0.2f - max_val) * clause_weights[cid];
      }
    }

    if (curr_energy < 0.05f) continue;

    float best_tunnel_energy = curr_energy;
    Vec4 best_tunnel_target = orig_pos;
    bool found_teleport = false;

    for (int r = 0; r < 16; ++r) {
      Vec4 target_p = orig_pos + tunnel_rays[r] * deep_tunnel_dist;
      float n = target_p.norm();
      if (n > 1e-4f) target_p = target_p * (1.0f / n);

      float tunnel_energy = 0.0f;
      for (int cid : c_indices) {
        int start = clause_starts[cid];
        int len = clause_lens[cid];
        float max_val = -2.0f;
        for (int k = 0; k < len; ++k) {
          int v = flat_vars[start + k];
          float px = (v == var) ? target_p.x : positions[v].x;
          float val = flat_signs[start + k] * px;
          if (val > max_val) max_val = val;
        }
        if (max_val < 0.2f) {
          tunnel_energy += (0.2f - max_val) * clause_weights[cid];
        }
      }

      if (tunnel_energy + min_energy_gain < best_tunnel_energy) {
        best_tunnel_energy = tunnel_energy;
        best_tunnel_target = target_p;
        found_teleport = true;
      }
    }

    if (found_teleport) {
      positions[var] = best_tunnel_target;
      velocities[var] = (best_tunnel_target - orig_pos) * 0.45f;
      latest_decisions[var] = (best_tunnel_target.x > 0.0f) ? 1 : 0;
    }
  }
}

void PhysicsSimulator::apply_ground_sonar_anchor(float basin_threshold, float snap_strength) {
  if (dim != 4 || clause_starts.empty() || active_vars.empty()) return;

  for (int var : active_vars) {
    const auto& c_indices = var_clauses[var];
    if (c_indices.empty()) continue;

    Vec4 p = positions[var];
    
    // Проекция строго вниз на дискретное дно русла (+X истина или -X ложь)
    float target_x = (p.x >= 0.0f) ? 1.0f : -1.0f;
    Vec4 p_floor(target_x, 0.0f, 0.0f, 0.0f);

    // Зондирование глубины потенциала на дне прямо под частицей
    float ground_energy = 0.0f;
    for (int cid : c_indices) {
      int start = clause_starts[cid];
      int len = clause_lens[cid];
      float max_val = -2.0f;
      for (int k = 0; k < len; ++k) {
        int v = flat_vars[start + k];
        float px = (v == var) ? target_x : positions[v].x;
        float val = flat_signs[start + k] * px;
        if (val > max_val) max_val = val;
      }
      if (max_val < 0.2f) {
        ground_energy += (0.2f - max_val) * clause_weights[cid];
      }
    }

    // Если на дне под частицей обнаружено чистое русло (E <= basin_threshold):
    if (ground_energy <= basin_threshold) {
      // 1. Гравитационный захват: примагничивание координаты ко дну
      positions[var] = p * (1.0f - snap_strength) + p_floor * snap_strength;
      
      // 2. Гашение тангенциальной скорости (сброс инерции пролета)
      velocities[var] = Vec4(0.0f, 0.0f, 0.0f, 0.0f);
      
      // 3. Фиксация решения в контрольной фазе
      latest_decisions[var] = (target_x > 0.0f) ? 1 : 0;
    }
  }
}




