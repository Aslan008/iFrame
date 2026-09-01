#include "cdcl_bridge.h"
#include "Solver.h"
#include <vector>
#include <memory>
#include <algorithm>
#include <cmath>

struct CDCLContext {
    int num_vars = 0;
    std::vector<std::vector<int>> clauses;
    bool vsids_mode = false;
    std::unique_ptr<CDCLSolver> solver;
    std::vector<int> last_assigns;
    int last_conflicts = 0;
    long long last_propagations = 0;
};

CDCLContext* cdcl_create(int32_t num_vars) {
    auto* ctx = new CDCLContext();
    ctx->num_vars = num_vars;
    return ctx;
}

void cdcl_add_clause(CDCLContext* ctx, const int32_t* lits, int32_t count) {
    if (!ctx) return;
    std::vector<int> cl;
    cl.reserve(count);
    for (int i = 0; i < count; ++i) {
        cl.push_back(lits[i]);
        ctx->num_vars = std::max(ctx->num_vars, std::abs(lits[i]));
    }
    ctx->clauses.push_back(std::move(cl));
}

void cdcl_set_vsids_mode(CDCLContext* ctx, int32_t enabled) {
    if (!ctx) return;
    ctx->vsids_mode = (enabled != 0);
}

int32_t cdcl_solve(CDCLContext* ctx, int32_t max_conflicts) {
    if (!ctx) return -1;
    ctx->solver = std::make_unique<CDCLSolver>(ctx->num_vars, ctx->clauses, max_conflicts > 0 ? max_conflicts : 100000);
    ctx->solver->vsids_mode = ctx->vsids_mode;
    SolveResult res = ctx->solver->solve();
    ctx->last_assigns = ctx->solver->assigns;
    ctx->last_conflicts = ctx->solver->conflicts;
    ctx->last_propagations = ctx->solver->propagations;

    switch (res) {
        case SolveResult::SAT: return 1;
        case SolveResult::UNSAT: return 0;
        default: return -1;
    }
}

int32_t cdcl_solve_assumptions(CDCLContext* ctx, const int32_t* assumptions, int32_t count, int32_t max_conflicts) {
    if (!ctx) return -1;
    std::vector<int> asmps;
    asmps.reserve(count);
    for (int i = 0; i < count; ++i) {
        asmps.push_back(assumptions[i]);
        ctx->num_vars = std::max(ctx->num_vars, std::abs(assumptions[i]));
    }
    ctx->solver = std::make_unique<CDCLSolver>(ctx->num_vars, ctx->clauses, asmps, max_conflicts > 0 ? max_conflicts : 100000);
    ctx->solver->vsids_mode = ctx->vsids_mode;
    SolveResult res = ctx->solver->solve();
    ctx->last_assigns = ctx->solver->assigns;
    ctx->last_conflicts = ctx->solver->conflicts;
    ctx->last_propagations = ctx->solver->propagations;

    switch (res) {
        case SolveResult::SAT: return 1;
        case SolveResult::UNSAT: return 0;
        default: return -1;
    }
}

int32_t cdcl_get_assignment(CDCLContext* ctx, int32_t var) {
    if (!ctx) return 0;
    int v = std::abs(var);
    if (v >= 0 && v < static_cast<int>(ctx->last_assigns.size())) {
        return ctx->last_assigns[v];
    }
    return 0;
}

int32_t cdcl_get_conflicts(CDCLContext* ctx) {
    return ctx ? ctx->last_conflicts : 0;
}

int64_t cdcl_get_propagations(CDCLContext* ctx) {
    return ctx ? ctx->last_propagations : 0;
}

void cdcl_destroy(CDCLContext* ctx) {
    delete ctx;
}
