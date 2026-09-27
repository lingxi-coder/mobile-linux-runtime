#include "LXISHExecutionPolicy.h"

#include <TargetConditionals.h>
#include <pthread.h>
#include <stddef.h>
#include <stdatomic.h>

#if !TARGET_OS_SIMULATOR
// Defined inside the patched libish socket layer. Keeping the version marker
// there makes a stale, unpatched libish fail to link instead of letting the
// bridge claim an enforcement receipt it cannot prove.
extern uint32_t lx_ish_sock_policy_hook_version(void);
extern uint64_t lx_ish_guest_execution_resident_bytes(uint64_t context);
extern int lx_ish_guest_execution_context_active(uint64_t context);
#endif

#define LXISH_MAX_EXECUTION_POLICIES 64

struct execution_policy_entry {
    uint64_t context;
    int network_policy;
};

static pthread_mutex_t policy_lock = PTHREAD_MUTEX_INITIALIZER;
static struct execution_policy_entry policies[LXISH_MAX_EXECUTION_POLICIES];
static _Atomic uint64_t next_context = UINT64_C(0x4c58000000000001);

uint32_t lx_ish_execution_policy_version(void) {
#if TARGET_OS_SIMULATOR
    return 0;
#else
    return lx_ish_sock_policy_hook_version();
#endif
}

uint64_t lx_ish_execution_context_next(void) {
    uint64_t value = atomic_fetch_add_explicit(&next_context, 1, memory_order_relaxed);
    if (value == 0) {
        value = atomic_fetch_add_explicit(&next_context, 1, memory_order_relaxed);
    }
    return value;
}

int lx_ish_execution_policy_register(uint64_t context, int network_policy) {
    if (context == 0 || network_policy < LXISHNetworkPolicyAllowed ||
        network_policy > LXISHNetworkPolicyLoopbackOnly) {
        return -1;
    }

    pthread_mutex_lock(&policy_lock);
    size_t vacant = LXISH_MAX_EXECUTION_POLICIES;
    for (size_t index = 0; index < LXISH_MAX_EXECUTION_POLICIES; index++) {
        if (policies[index].context == context) {
            pthread_mutex_unlock(&policy_lock);
            return -1;
        }
        if (vacant == LXISH_MAX_EXECUTION_POLICIES && policies[index].context == 0) {
            vacant = index;
        }
    }
    if (vacant == LXISH_MAX_EXECUTION_POLICIES) {
        pthread_mutex_unlock(&policy_lock);
        return -1;
    }
    policies[vacant].network_policy = network_policy;
    policies[vacant].context = context;
    pthread_mutex_unlock(&policy_lock);
    return 0;
}

void lx_ish_execution_policy_unregister(uint64_t context) {
    if (context == 0) {
        return;
    }
    pthread_mutex_lock(&policy_lock);
    for (size_t index = 0; index < LXISH_MAX_EXECUTION_POLICIES; index++) {
        if (policies[index].context == context) {
            policies[index].context = 0;
            policies[index].network_policy = LXISHNetworkPolicyAllowed;
            break;
        }
    }
    pthread_mutex_unlock(&policy_lock);
}

int lx_ish_network_policy_for_context(uint64_t context) {
    // Context zero belongs to ordinary interactive iSH tasks. Every non-zero
    // context is minted by this registry, so a missing entry means ownership
    // was lost unexpectedly and must fail closed instead of regaining network.
    int result = context == 0 ? LXISHNetworkPolicyAllowed : LXISHNetworkPolicyDisabled;
    if (context == 0) {
        return result;
    }
    pthread_mutex_lock(&policy_lock);
    for (size_t index = 0; index < LXISH_MAX_EXECUTION_POLICIES; index++) {
        if (policies[index].context == context) {
            result = policies[index].network_policy;
            break;
        }
    }
    pthread_mutex_unlock(&policy_lock);
    return result;
}

uint64_t lx_ish_execution_resident_bytes(uint64_t context) {
#if TARGET_OS_SIMULATOR
    (void)context;
    return 0;
#else
    return lx_ish_guest_execution_resident_bytes(context);
#endif
}

int lx_ish_execution_context_active(uint64_t context) {
#if TARGET_OS_SIMULATOR
    (void)context;
    return 0;
#else
    return lx_ish_guest_execution_context_active(context);
#endif
}
