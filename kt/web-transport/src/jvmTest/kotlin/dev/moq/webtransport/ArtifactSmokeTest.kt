package dev.moq.webtransport

import com.sun.jna.Native
import com.sun.jna.Platform
import java.io.File
import java.net.JarURLConnection
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test
import uniffi.web_transport.Client
import uniffi.web_transport.ClientConfig
import uniffi.web_transport.CongestionControl
import uniffi.web_transport.Server
import uniffi.web_transport.ServerConfig
import uniffi.web_transport.UniffiLib

class ArtifactSmokeTest {
    @Test
    fun packagedClientServerLifecycle() = runBlocking {
        withTimeout(30_000) {
            val artifact = File(Client::class.java.protectionDomain.codeSource.location.toURI())
            assertTrue(artifact.isFile && artifact.extension == "jar", "Classes must load from the JVM JAR: $artifact")
            val resourcePath = "${Platform.RESOURCE_PREFIX}/${System.mapLibraryName("web_transport_ffi")}"
            val resource = assertNotNull(
                Client::class.java.classLoader.getResource(resourcePath),
                "Missing native resource $resourcePath in $artifact",
            )
            assertEquals("jar", resource.protocol, "Native resource must be packaged in the JAR")
            assertEquals(artifact.toURI(), (resource.openConnection() as JarURLConnection).jarFileURL.toURI())

            Client(ClientConfig(
                noCertVerification = true,
                congestionControl = CongestionControl.DEFAULT,
            )).use { client ->
                // UniFFI has now loaded the library. Require JNA's extracted resource path.
                val loaded = Native.getNativeLibrary(UniffiLib::class.java).file
                assertEquals(File(System.getProperty("jna.tmpdir")).canonicalFile, loaded.canonicalFile.parentFile)
                println("Loaded native resource from $resource via $loaded")

                try {
                    Server(ServerConfig(
                        certificateChain = listOf(File(System.getenv("WEB_TRANSPORT_TEST_CERT")).readBytes()),
                        privateKey = File(System.getenv("WEB_TRANSPORT_TEST_KEY")).readBytes(),
                        bind = "127.0.0.1:0",
                        congestionControl = CongestionControl.DEFAULT,
                    )).use { server ->
                        try {
                            val url = "https://127.0.0.1:${server.localAddr().port}/kotlin-smoke"
                            val accepted = async {
                                assertNotNull(server.accept()).use { request ->
                                    assertEquals(url, request.url())
                                    request.accept()
                                }
                            }
                            client.connect(url).use { outgoing ->
                                accepted.await().use { incoming ->
                                    try {
                                        assertFalse(outgoing.isClosed())
                                        assertFalse(incoming.isClosed())
                                        val payload = "Kotlin JVM artifact smoke".encodeToByteArray()
                                        val stream = outgoing.openBi()
                                        stream.send.use { send ->
                                            stream.recv.use { recv ->
                                                send.write(payload)
                                                send.finish()
                                                val peer = incoming.acceptBi()
                                                peer.send.use { reply ->
                                                    peer.recv.use { request ->
                                                        assertContentEquals(payload, request.readToEnd(1024uL))
                                                        reply.write(payload)
                                                        reply.finish()
                                                        assertContentEquals(payload, recv.readToEnd(1024uL))
                                                    }
                                                }
                                            }
                                        }
                                        outgoing.close(0u, "smoke complete")
                                        outgoing.waitClosed()
                                        incoming.waitClosed()
                                        assertTrue(outgoing.isClosed())
                                        assertTrue(incoming.isClosed())
                                    } finally {
                                        outgoing.close(0u, "smoke complete")
                                        incoming.close(0u, "smoke cleanup")
                                    }
                                }
                            }
                        } finally {
                            server.close(0uL, "smoke cleanup")
                            server.waitClosed()
                        }
                        assertNull(server.accept(), "Closed server must stop accepting")
                    }
                } finally {
                    client.close(0uL, "smoke cleanup")
                    client.waitClosed()
                }
            }
        }
    }
}
