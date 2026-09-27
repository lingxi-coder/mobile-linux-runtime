import MobileLinuxRuntime
import MobileLinuxRuntimeBindings

/// A clean application integration point: no Harness, client protocol or host preferences.
public func makeSampleRuntime(configuration: RuntimeConfig) throws -> MobileLinuxRuntime {
    try MobileLinuxRuntime(configuration: configuration)
}
