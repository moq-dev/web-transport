// swift-tools-version:5.9
//
// Swift Package Manager manifest for WebTransport.
//
// The XCFramework is attached to the matching web-transport-ffi-v* GitHub Release.
// Run `just test-swift` from the repository root to test an isolated host package
// without a published release or changes to this release manifest.

import PackageDescription

let package = Package(
    name: "WebTransport",
    platforms: [
        .iOS(.v15),
        .macOS(.v12),
    ],
    products: [
        .library(name: "WebTransport", targets: ["WebTransport"]),
    ],
    targets: [
        .target(
            name: "WebTransport",
            dependencies: ["WebTransportFFI"],
            path: "Sources/WebTransport"
        ),
        .binaryTarget(
            name: "WebTransportFFI",
            url: "https://github.com/moq-dev/web-transport/releases/download/web-transport-ffi-vREPLACE_VERSION/WebTransportFFI.xcframework.zip",
            checksum: "REPLACE_CHECKSUM"
        ),
        .testTarget(
            name: "WebTransportTests",
            dependencies: ["WebTransport"],
            path: "Tests/WebTransportTests"
        ),
    ]
)
