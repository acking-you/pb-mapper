package com.pb_mapper.ui

import java.security.InvalidKeyException
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.*
import org.junit.Test

class ComputerKeystoreTest {
    private val key = SecretKeySpec(ByteArray(32) { 1 }, "AES")
    private val other = SecretKeySpec(ByteArray(32) { 2 }, "AES")
    private val plain = ByteArray(32) { it.toByte() }
    private val wrapped = Cipher.getInstance("AES/GCM/NoPadding").run {
        init(Cipher.ENCRYPT_MODE, key)
        iv + doFinal(plain)
    }

    @Test
    fun onlyAKeyThatCanNeverOpenTheWrappedCopyIsGone() {
        assertArrayEquals(plain, ComputerKeystore.open(wrapped, key))
        // These start the vault over: no retry could ever open it.
        for ((input, with) in listOf(wrapped to null, wrapped to other, wrapped.copyOf(20) to key)) {
            assertThrows(ComputerKeystore.KeyGone::class.java) { ComputerKeystore.open(input, with) }
        }
        // Any other failure is not a loss: the next load tries again.
        assertThrows(InvalidKeyException::class.java) {
            ComputerKeystore.open(wrapped, SecretKeySpec(ByteArray(32), "HmacSHA256"))
        }
    }
}
