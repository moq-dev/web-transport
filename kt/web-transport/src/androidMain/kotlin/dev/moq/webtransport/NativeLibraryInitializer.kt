package dev.moq.webtransport

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.util.Log

/**
 * Loads the native library while the app starts, after its Application exists.
 *
 * `System.loadLibrary` runs the library's `JNI_OnLoad`, which initializes the
 * OS platform certificate verifier from the application Context. JNA, which the
 * generated bindings use, loads the library without running `JNI_OnLoad`.
 */
internal class NativeLibraryInitializer : ContentProvider() {
    override fun onCreate(): Boolean {
        try {
            System.loadLibrary("web_transport_ffi")
        } catch (e: UnsatisfiedLinkError) {
            // Not fatal at startup: JNA reports the load failure when the bindings are first used.
            Log.w("WebTransport", "failed to load web_transport_ffi", e)
        }
        return true
    }

    override fun query(
        uri: Uri,
        projection: Array<out String>?,
        selection: String?,
        selectionArgs: Array<out String>?,
        sortOrder: String?,
    ): Cursor? = null

    override fun getType(uri: Uri): String? = null

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0

    override fun update(
        uri: Uri,
        values: ContentValues?,
        selection: String?,
        selectionArgs: Array<out String>?,
    ): Int = 0
}
