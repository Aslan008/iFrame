#pragma once
#include <vector>
#include <string>
#include <cstdint>

struct GpuSearchResult {
    bool found = false;
    std::string preimage;
    double time_s = 0.0;
    uint64_t hashes_computed = 0;
    double mhashes_per_sec = 0.0;
};

struct GpuCubeFilterResult {
    std::vector<std::vector<int>> valid_cubes;
    uint64_t total_tested = 0;
    uint64_t surviving_cubes = 0;
    double time_s = 0.0;
};

bool is_cuda_available();
std::string get_gpu_name();

GpuSearchResult gpu_sha256_search(
    const std::string& target_hex,
    int msg_len,
    int rounds,
    int prefix_len,
    const std::string& known_prefix = ""
);

GpuCubeFilterResult gpu_generate_filtered_cubes(
    const std::string& target_hex,
    int msg_len,
    int rounds,
    const std::vector<std::vector<int>>& char_vars,
    int max_cubes = 1024,
    int prefix_chars = 2
);
