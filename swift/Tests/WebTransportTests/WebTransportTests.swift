import WebTransport
import XCTest

final class WebTransportTests: XCTestCase {
    func testInvalidClientConfig() throws {
        XCTAssertThrowsError(try Client(config: ClientConfig(
            serverCertificateHashes: [],
            noCertVerification: true,
            congestionControl: .default
        ))) { error in
            guard let bindingError = error as? WebTransportError, case .InvalidArgument = bindingError else {
                return XCTFail("Expected InvalidArgument, got \(error)")
            }
        }
    }
}
