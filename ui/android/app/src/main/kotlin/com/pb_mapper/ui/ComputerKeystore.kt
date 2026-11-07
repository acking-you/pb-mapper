package com.pb_mapper.ui

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMethodCodec
import java.security.KeyStore
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
 * Any failure, a key lost to a device restore included, is a `KEYSTORE` error.
 */
object ComputerKeystore {
    private const val CHANNEL = "pb_mapper/keystore"
    private const val ALIAS = "pb_mapper.computers.v1"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val IV_BYTES = 12
    private const val TAG_BITS = 128

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
                    "unwrap" -> result.success(unwrap(input))
                    else -> result.notImplemented()
                }
            } catch (error: Exception) {
                result.error("KEYSTORE", error.message ?: error.javaClass.simpleName, null)
            }
        }
    }

    private fun wrap(plain: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key(create = true))
        return cipher.iv + cipher.doFinal(plain)
    }

    private fun unwrap(wrapped: ByteArray): ByteArray {
        require(wrapped.size > IV_BYTES) { "the wrapped key is too short" }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key(create = false), GCMParameterSpec(TAG_BITS, wrapped, 0, IV_BYTES))
        return cipher.doFinal(wrapped, IV_BYTES, wrapped.size - IV_BYTES)
    }

    /** The Keystore key, created by the first `wrap`; never extractable. */
    private fun key(create: Boolean): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        check(create) { "the remembered computers key is gone" }
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
