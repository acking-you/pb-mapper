package com.pb_mapper.ui

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMethodCodec
import java.security.KeyStore
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Wraps the remembered computers' data key with an Android Keystore key, so
 * the wrapped copy Rust keeps beside the vault opens nothing off this device.
 * Dart sends the raw key through once per process; the vault, and every
 * invitation in it, stays in Rust.
 *
 * `wrap` and `unwrap` take and return bytes: IV (12) ‖ ciphertext ‖ tag.
 * An `unwrap` that can never succeed, because the Keystore key is gone or no
 * longer opens the wrapped copy, is a `KEY_GONE` error, and only that one
 * lets Dart start the vault over. Any other failure, a busy Keystore
 * included, is a `KEYSTORE` error, and the next load tries again.
 */
object ComputerKeystore {
    private const val CHANNEL = "pb_mapper/keystore"
    private const val ALIAS = "pb_mapper.computers.v1"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val IV_BYTES = 12
    private const val TAG_BITS = 128

    /** What a wrapped key was sealed with is gone for good. */
    internal class KeyGone(message: String, cause: Throwable? = null) : Exception(message, cause)

    fun register(messenger: BinaryMessenger) {
        // Keystore calls can block on secure hardware, so they run off the
        // platform thread.
        val channel = MethodChannel(
            messenger,
            CHANNEL,
            StandardMethodCodec.INSTANCE,
            messenger.makeBackgroundTaskQueue(),
        )
        channel.setMethodCallHandler { call, result ->
            try {
                val input = call.arguments as? ByteArray
                    ?: throw IllegalArgumentException("expected bytes")
                when (call.method) {
                    "wrap" -> result.success(wrap(input))
                    "unwrap" -> result.success(open(input, storedKey()))
                    else -> result.notImplemented()
                }
            } catch (error: KeyGone) {
                result.error("KEY_GONE", error.message, null)
            } catch (error: Exception) {
                result.error("KEYSTORE", error.message ?: error.javaClass.simpleName, null)
            }
        }
    }

    private fun wrap(plain: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, storedKey() ?: newKey())
        return cipher.iv + cipher.doFinal(plain)
    }

    /**
     * Opens [wrapped] with [key], the Keystore key or null when it is gone.
     * Throws [KeyGone] only when no retry could ever open it.
     */
    internal fun open(wrapped: ByteArray, key: SecretKey?): ByteArray {
        if (wrapped.size < IV_BYTES + TAG_BITS / 8) throw KeyGone("the wrapped key is too short")
        if (key == null) throw KeyGone("the remembered computers key is gone")
        try {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, wrapped, 0, IV_BYTES))
            return cipher.doFinal(wrapped, IV_BYTES, wrapped.size - IV_BYTES)
        } catch (error: KeyPermanentlyInvalidatedException) {
            throw KeyGone("the remembered computers key was invalidated", error)
        } catch (error: AEADBadTagException) {
            throw KeyGone("the remembered computers key does not open its wrapped copy", error)
        }
    }

    /** The Keystore key, or null when there is none. */
    private fun storedKey(): SecretKey? {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        return store.getKey(ALIAS, null) as? SecretKey
    }

    /** Created by the first `wrap`; never extractable. */
    private fun newKey(): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .build(),
        )
        return generator.generateKey()
    }
}
