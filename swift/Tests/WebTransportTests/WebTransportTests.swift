import Darwin
import Foundation
import WebTransport
import XCTest
import web_transportFFI

final class WebTransportTests: XCTestCase {
    func testNativeLibraryOrigin() throws {
        let expected = try XCTUnwrap(ProcessInfo.processInfo.environment["WEB_TRANSPORT_TEST_LIBRARY"])
        let symbol = unsafeBitCast(
            ffi_web_transport_ffi_uniffi_contract_version as @convention(c) () -> UInt32,
            to: UnsafeRawPointer.self
        )
        var info = Dl_info()
        XCTAssertNotEqual(dladdr(symbol, &info), 0)
        let filename = try XCTUnwrap(info.dli_fname)
        let loaded = URL(fileURLWithPath: String(cString: filename)).resolvingSymlinksInPath().path
        XCTAssertEqual(loaded, URL(fileURLWithPath: expected).resolvingSymlinksInPath().path)
        print("Swift native library: \(loaded)")
    }

    func testLoopbackLifecycle() async throws {
        let environment = ProcessInfo.processInfo.environment
        let certPath = try XCTUnwrap(environment["WEB_TRANSPORT_TEST_CERT"])
        let keyPath = try XCTUnwrap(environment["WEB_TRANSPORT_TEST_KEY"])
        let server = try Server(config: ServerConfig(
            certificateChain: [try Data(contentsOf: URL(fileURLWithPath: certPath))],
            privateKey: try Data(contentsOf: URL(fileURLWithPath: keyPath)),
            bind: "[::1]:0",
            congestionControl: .default
        ))
        addTeardownBlock {
            try server.close(code: 0, reason: "Swift test cleanup")
            await server.waitClosed()
        }
        let client = try Client(config: ClientConfig(noCertVerification: true, congestionControl: .default))
        addTeardownBlock {
            try client.close(code: 0, reason: "Swift test cleanup")
            await client.waitClosed()
        }

        let url = "https://[::1]:\(server.localAddr().port)/swift-test"
        let connection = Task { try await client.connect(url: url) }
        defer { connection.cancel() }
        let incoming = await server.accept()
        let request = try XCTUnwrap(incoming)
        XCTAssertEqual(request.url(), url)
        let peer = try await request.accept()
        let session = try await connection.value
        // Keep both Session owners alive until all stream/datagram work ends.
        addTeardownBlock {
            session.close(code: 0, reason: "Swift test cleanup")
            peer.close(code: 0, reason: "Swift test cleanup")
            await session.waitClosed()
            await peer.waitClosed()
        }
        XCTAssertFalse(session.isClosed())
        XCTAssertNil(session.closeReason())

        print("Swift lifecycle: bidirectional stream and FIN")
        let payload = Data([0, 1, 127, 128, 255])
        let outgoing = try await session.openBi()
        try await outgoing.send.write(data: payload)
        try await outgoing.send.finish()
        let accepted = try await peer.acceptBi()
        let received = try await accepted.recv.readToEnd(limit: 1024)
        XCTAssertEqual(received, payload)
        try await accepted.send.write(data: received)
        try await accepted.send.finish()
        let echo = try await outgoing.recv.readToEnd(limit: 1024)
        XCTAssertEqual(echo, payload)
        let sendClosed = try await outgoing.send.waitClosed()
        XCTAssertNil(sendClosed)

        print("Swift lifecycle: server-initiated unidirectional stream and EOF")
        let uni = try await peer.openUni()
        try await uni.write(data: payload)
        try await uni.finish()
        let reader = try await session.acceptUni()
        let uniData = try await reader.readExact(n: UInt64(payload.count))
        XCTAssertEqual(uniData, payload)
        let eof = try await reader.read(n: 1)
        XCTAssertTrue(eof.isEmpty)

        print("Swift lifecycle: datagrams in both directions")
        XCTAssertGreaterThan(session.maxDatagramSize(), UInt64(payload.count))
        try session.sendDatagram(data: payload)
        let datagram = try await peer.receiveDatagram()
        XCTAssertEqual(datagram, payload)
        try peer.sendDatagram(data: datagram)
        let reply = try await session.receiveDatagram()
        XCTAssertEqual(reply, payload)
        do {
            try session.sendDatagram(data: Data(repeating: 0, count: 65_536))
            XCTFail("Oversized datagram was accepted")
        } catch let error as WebTransportError {
            XCTAssertEqual(error, .DatagramTooLarge)
        }

        print("Swift lifecycle: explicit close and concurrent receiver failure")
        let pending = Task { try await peer.receiveDatagram() }
        defer { pending.cancel() }
        session.close(code: 42, reason: "Swift test complete")
        await session.waitClosed()
        await peer.waitClosed()
        XCTAssertTrue(session.isClosed())
        XCTAssertTrue(peer.isClosed())
        XCTAssertEqual(session.closeReason(), .SessionClosedLocally)
        let reason = try XCTUnwrap(peer.closeReason())
        guard case let .SessionClosedByPeer(_, code, message) = reason else {
            return XCTFail("Unexpected peer close reason: \(reason)")
        }
        XCTAssertEqual(code, 42)
        XCTAssertEqual(message, "Swift test complete")
        do {
            _ = try await pending.value
            XCTFail("Datagram receive succeeded after closure")
        } catch let error as WebTransportError {
            XCTAssertEqual(error, reason)
        }
    }
}
