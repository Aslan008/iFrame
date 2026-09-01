#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CDCLContext CDCLContext;

CDCLContext* cdcl_create(int32_t num_vars);
void cdcl_add_clause(CDCLContext* ctx, const int32_t* lits, int32_t count);
void cdcl_set_vsids_mode(CDCLContext* ctx, int32_t enabled);
int32_t cdcl_solve(CDCLContext* ctx, int32_t max_conflicts);
int32_t cdcl_solve_assumptions(CDCLContext* ctx, const int32_t* assumptions, int32_t count, int32_t max_conflicts);
int32_t cdcl_get_assignment(CDCLContext* ctx, int32_t var);
int32_t cdcl_get_conflicts(CDCLContext* ctx);
int64_t cdcl_get_propagations(CDCLContext* ctx);
void cdcl_destroy(CDCLContext* ctx);

#ifdef __cplusplus
}
#endif
