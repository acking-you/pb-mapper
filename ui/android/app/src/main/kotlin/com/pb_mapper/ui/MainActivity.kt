package com.pb_mapper.ui

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

class MainActivity : FlutterActivity() {
    private var updates: UpdateInstaller? = null
    private var desktopVideo: DesktopVideo? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        updates = UpdateInstaller(this, flutterEngine.dartExecutor.binaryMessenger)
        desktopVideo = DesktopVideo(flutterEngine.dartExecutor.binaryMessenger, flutterEngine.renderer)
        ComputerKeystore.register(flutterEngine.dartExecutor.binaryMessenger)
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        updates?.dispose()
        updates = null
        desktopVideo?.dispose()
        desktopVideo = null
        super.cleanUpFlutterEngine(flutterEngine)
    }

    // Back at the root leaves the app running, as Home does. FlutterActivity
    // would finish instead, destroying the engine but not the native handle,
    // whose tunnels and paused viewer would outlive it unseen.
    override fun popSystemNavigator(): Boolean {
        moveTaskToBack(true)
        return true
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        updates?.onActivityResult(requestCode)
    }
}
