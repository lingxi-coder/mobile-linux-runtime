import MobileLinuxRuntimeBindings

/// The app chooses paths, authorization, mount policy and rootfs distribution.
/// Keep this object alive while using its task, PTY and raw-stdio handles.
public final class MobileLinuxRuntime {
    public let handle: RuntimeHandle
    public init(configuration: RuntimeConfig) throws {
        self.handle = try createRuntime(config: configuration)
    }
    public func boot() async throws -> MobileLinuxStatusFfi { try await handle.boot() }
    public func shutdown() async throws { try await handle.shutdown() }
    public func capability() async -> MobileLinuxCapabilityFfi { await handle.capability() }
}
