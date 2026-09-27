#ifndef LXISHExecutionPolicy_h
#define LXISHExecutionPolicy_h

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
    LXISHNetworkPolicyAllowed = 0,
    LXISHNetworkPolicyDisabled = 1,
    LXISHNetworkPolicyLoopbackOnly = 2,
};

/// Version is non-zero only when the linked iSH socket implementation contains
/// LingXi's syscall hook as well as this host registry.
uint32_t lx_ish_execution_policy_version(void);
uint64_t lx_ish_execution_context_next(void);

/// Register before task_start. The opaque context is stamped on the guest task
/// group by ISHShellExecutor and inherited by every child.
int lx_ish_execution_policy_register(uint64_t context, int network_policy);
void lx_ish_execution_policy_unregister(uint64_t context);

/// Called by the patched iSH socket syscall layer.
int lx_ish_network_policy_for_context(uint64_t context);

/// Conservative guest-backed RSS for all distinct address spaces carrying the
/// execution context. Shared address spaces are counted once; backed pages in
/// separate processes are summed like process-group RSS.
uint64_t lx_ish_execution_resident_bytes(uint64_t context);
int lx_ish_execution_context_active(uint64_t context);

#ifdef __cplusplus
}
#endif

#endif /* LXISHExecutionPolicy_h */
