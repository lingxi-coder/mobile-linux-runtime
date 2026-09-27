// LingXi LoopbackOnly policy for the Android PRoot tracee tree.
//
// The Android build copies this file over PRoot's legacy `port_switch`
// extension. PRoot enables it only when its fixed host invocation contains
// `-p`. Every guest socket syscall is inspected before it reaches the host
// kernel, and the extension is inherited by every tracee child.

#include <errno.h>
#include <fcntl.h>
#include <linux/net.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#include "extension/extension.h"
#include "tracee/mem.h"
#include "tracee/tracee.h"

static int write_all(int fd, const char *value, size_t length)
{
	size_t cursor = 0;

	while (cursor < length) {
		ssize_t written = write(fd, value + cursor, length - cursor);
		if (written < 0) {
			if (errno == EINTR)
				continue;
			return -errno;
		}
		if (written == 0)
			return -EIO;
		cursor += (size_t) written;
	}
	return 0;
}

/*
 * Publish proof only after this sockaddr-aware extension is initialized.
 * Failure to publish must not remove the extension: Rust will fail the launch
 * closed when the receipt is absent, while the tracee remains restricted in
 * the meantime.
 */
static void publish_enforcement_receipt(void)
{
	static const char receipt[] = "loopback_only\n";
	const char *path = getenv("LINGXI_ENFORCEMENT_RECEIPT_PATH");
	int fd;

	if (path == NULL || path[0] == '\0')
		return;
	fd = open(path, O_WRONLY | O_TRUNC | O_CLOEXEC | O_NOFOLLOW);
	if (fd >= 0) {
		if (write_all(fd, receipt, sizeof(receipt) - 1) == 0)
			(void) fsync(fd);
		(void) close(fd);
	}
	(void) unsetenv("LINGXI_ENFORCEMENT_RECEIPT_PATH");
}

static int validate_socket_domain(word_t domain)
{
	switch ((int) domain) {
	case AF_UNIX:
	case AF_INET:
	case AF_INET6:
		return 0;
	default:
		return -EPERM;
	}
}

static int validate_sockaddr(Tracee *tracee, word_t address, word_t length)
{
	struct sockaddr_storage storage;
	size_t copy_length;
	int status;

	if (address == 0)
		return 0;
	if (length < sizeof(sa_family_t))
		return -EPERM;

	memset(&storage, 0, sizeof(storage));
	copy_length = (size_t) length;
	if (copy_length > sizeof(storage))
		copy_length = sizeof(storage);
	status = read_data(tracee, &storage, address, copy_length);
	if (status < 0)
		return status;

	switch (storage.ss_family) {
	case AF_UNIX:
		return 0;
	case AF_INET: {
		const struct sockaddr_in *ipv4 = (const struct sockaddr_in *) &storage;
		if (length < sizeof(*ipv4))
			return -EPERM;
		return (ntohl(ipv4->sin_addr.s_addr) >> 24) == 127 ? 0 : -EPERM;
	}
	case AF_INET6: {
		static const struct in6_addr loopback = IN6ADDR_LOOPBACK_INIT;
		const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *) &storage;
		if (length < sizeof(*ipv6))
			return -EPERM;
		return memcmp(&ipv6->sin6_addr, &loopback, sizeof(loopback)) == 0
			? 0
			: -EPERM;
	}
	default:
		return -EPERM;
	}
}

static int validate_sendmsg(Tracee *tracee, word_t header_address)
{
	struct msghdr header;
	int status;

	if (header_address == 0)
		return -EFAULT;
	memset(&header, 0, sizeof(header));
	status = read_data(tracee, &header, header_address, sizeof(header));
	if (status < 0)
		return status;
	return validate_sockaddr(tracee, (word_t) header.msg_name,
				 (word_t) header.msg_namelen);
}

/* socketcall is relevant only to a 32-bit tracee. The shipped runtime is
 * 64-bit, but handling its simple cases prevents a future compat loader from
 * silently bypassing the policy. Complex batched messages remain fail-closed.
 */
static int validate_socketcall(Tracee *tracee)
{
	uint32_t arguments[6] = { 0 };
	int call = (int) peek_reg(tracee, ORIGINAL, SYSARG_1);
	int status = read_data(tracee, arguments,
			       peek_reg(tracee, ORIGINAL, SYSARG_2),
			       sizeof(arguments));

	if (status < 0)
		return status;
	switch (call) {
	case SYS_SOCKET:
	case SYS_SOCKETPAIR:
		return validate_socket_domain(arguments[0]);
	case SYS_BIND:
	case SYS_CONNECT:
		return validate_sockaddr(tracee, arguments[1], arguments[2]);
	case SYS_SENDTO:
		return validate_sockaddr(tracee, arguments[4], arguments[5]);
	case SYS_SENDMSG:
	case SYS_SENDMMSG:
		return -EPERM;
	default:
		return 0;
	}
}

int port_switch_callback(Extension *extension, ExtensionEvent event,
			 intptr_t data1 UNUSED, intptr_t data2 UNUSED)
{
	switch (event) {
	case INITIALIZATION: {
		static FilteredSysnum filtered_sysnums[] = {
			{ PR_socket, 0 },
			{ PR_socketpair, 0 },
			{ PR_bind, 0 },
			{ PR_connect, 0 },
			{ PR_sendto, 0 },
			{ PR_sendmsg, 0 },
			{ PR_sendmmsg, 0 },
			{ PR_socketcall, 0 },
			FILTERED_SYSNUM_END
		};

		extension->filtered_sysnums = filtered_sysnums;
		publish_enforcement_receipt();
		return 0;
	}
	case INHERIT_PARENT:
		return 0;
	case SYSCALL_ENTER_START: {
		Tracee *tracee = TRACEE(extension);

		switch (get_sysnum(tracee, ORIGINAL)) {
		case PR_socket:
		case PR_socketpair:
			return validate_socket_domain(
				peek_reg(tracee, ORIGINAL, SYSARG_1));
		case PR_bind:
		case PR_connect:
			return validate_sockaddr(
				tracee,
				peek_reg(tracee, ORIGINAL, SYSARG_2),
				peek_reg(tracee, ORIGINAL, SYSARG_3));
		case PR_sendto:
			return validate_sockaddr(
				tracee,
				peek_reg(tracee, ORIGINAL, SYSARG_5),
				peek_reg(tracee, ORIGINAL, SYSARG_6));
		case PR_sendmsg:
			return validate_sendmsg(
				tracee, peek_reg(tracee, ORIGINAL, SYSARG_2));
		case PR_sendmmsg:
			return -EPERM;
		case PR_socketcall:
			return validate_socketcall(tracee);
		default:
			return 0;
		}
	}
	default:
		return 0;
	}
}
