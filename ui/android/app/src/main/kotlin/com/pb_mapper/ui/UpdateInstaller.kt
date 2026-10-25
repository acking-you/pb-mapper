package com.pb_mapper.ui

import android.app.Activity
import android.content.ClipData
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.FileProvider
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import java.io.File

/** Only our verified cache file is shared. Android keeps final install consent. */
class UpdateInstaller(private val activity: Activity, messenger: BinaryMessenger) {
    private val channel = MethodChannel(messenger, "pb_mapper/updates")
    private var pending: Pair<File, MethodChannel.Result>? = null

    init {
        channel.setMethodCallHandler { call, result ->
            when (call.method) {
                "abis" -> result.success(Build.SUPPORTED_ABIS.toList())
                "install" -> {
                    if (pending != null) {
                        result.error("install_busy", "An install permission request is pending", null)
                    } else {
                        try {
                            val path = requireNotNull(call.argument<String>("path"))
                            val file = File(path).canonicalFile
                            validate(file)
                            if (canInstall()) {
                                openInstaller(file, result)
                            } else {
                                pending = file to result
                                @Suppress("DEPRECATION")
                                activity.startActivityForResult(
                                    Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                                        Uri.parse("package:${activity.packageName}")),
                                    INSTALL_PERMISSION,
                                )
                            }
                        } catch (error: Exception) {
                            pending = null
                            fail(result, error)
                        }
                    }
                }
                else -> result.notImplemented()
            }
        }
    }

    fun onActivityResult(requestCode: Int) {
        if (requestCode != INSTALL_PERMISSION) return
        val (file, result) = pending ?: return
        pending = null
        if (!canInstall()) {
            result.error("permission_denied", "Allow installs from pb-mapper, then retry", null)
            return
        }
        try {
            // Identity and signing are checked again after returning from Settings.
            validate(file)
            openInstaller(file, result)
        } catch (error: Exception) {
            fail(result, error)
        }
    }

    fun dispose() {
        channel.setMethodCallHandler(null)
        pending?.second?.error("install_cancelled", "The activity was closed", null)
        pending = null
    }

    private fun canInstall() = Build.VERSION.SDK_INT < Build.VERSION_CODES.O ||
        activity.packageManager.canRequestPackageInstalls()

    @Suppress("DEPRECATION")
    private fun validate(file: File) {
        val cache = File(activity.cacheDir, "pb-mapper-updates").canonicalFile
        require(file.isFile && file.extension == "apk" &&
            file.parentFile?.parentFile == cache &&
            file.parentFile?.name?.startsWith("download-") == true) {
            "The package must be an APK in the app's private update cache"
        }
        val pm = activity.packageManager
        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
            PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES
        val candidate = pm.getPackageArchiveInfo(file.path, flags)
            ?: throw IncompatiblePackage("Android could not read this APK")
        val current = pm.getPackageInfo(activity.packageName, flags)
        if (candidate.packageName != activity.packageName ||
            code(candidate) <= code(current)) {
            throw IncompatiblePackage("The APK must be a newer build of ${activity.packageName}")
        }
        // Release signing is deliberately stable. Never make an unrelated app
        // installable just because its bytes match a release manifest.
        if (signers(candidate).isEmpty() || signers(candidate) != signers(current)) {
            throw IncompatiblePackage("The APK signing certificate differs from the installed app. " +
                "Older CI releases used disposable keys; export your settings before migrating.")
        }
    }

    @Suppress("DEPRECATION")
    private fun signers(info: PackageInfo): Set<String> {
        val signatures = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
            info.signingInfo?.apkContentsSigners else info.signatures
        return signatures?.map { it.toCharsString() }?.toSet() ?: emptySet()
    }

    @Suppress("DEPRECATION")
    private fun code(info: PackageInfo): Long =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) info.longVersionCode
        else info.versionCode.toLong()

    private fun openInstaller(file: File, result: MethodChannel.Result) {
        val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.updates", file)
        activity.startActivity(Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            clipData = ClipData.newRawUri("pb-mapper update", uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        })
        result.success(null)
    }

    private fun fail(result: MethodChannel.Result, error: Exception) {
        result.error(if (error is IncompatiblePackage) "incompatible_package" else "install_failed",
            error.message, null)
    }

    private class IncompatiblePackage(message: String) : Exception(message)

    companion object {
        private const val INSTALL_PERMISSION = 9701
    }
}
