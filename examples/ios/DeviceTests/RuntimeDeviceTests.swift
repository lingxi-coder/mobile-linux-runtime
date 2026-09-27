import XCTest
@testable import MobileLinuxSample

final class RuntimeDeviceTests: XCTestCase {
    func testStandaloneDeviceRuntimeContract() async throws {
        #if targetEnvironment(simulator)
        throw XCTSkip("Real kernel execution requires a physical arm64 device")
        #else
        let resources = try XCTUnwrap(Bundle(for: Self.self).resourceURL).appendingPathComponent("DeviceResources")
        try await SDKDeviceSmoke().run(resources: resources)
        #endif
    }
}
